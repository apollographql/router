use std::pin::Pin;
use std::sync::Arc;

use futures::lock::Mutex;
use futures::stream::{empty, iter, select_all};
use futures::{FutureExt, Stream, StreamExt, TryFutureExt};
use query_planner::model::{FetchNode, FlattenNode, PlanNode, QueryPlan};
use query_planner::{QueryPlanOptions, QueryPlanner, QueryPlannerError};
use serde_json::to_string_pretty;
use serde_json::{Map, Value};

use crate::traverser::Traverser;
use crate::{
    FetchError, GraphQLFetcher, GraphQLRequest, GraphQLResponse, GraphQLResponseStream, Path,
    ServiceRegistry,
};

type TraverserStream = Pin<Box<dyn Stream<Item = Traverser> + Send>>;

/// Federated graph fetcher creates a query plan and executes the plan against one or more
/// subgraphs.
#[derive(Clone, Debug)]
pub struct FederatedGraph {
    query_planner: Arc<Mutex<dyn QueryPlanner>>,
    service_registry: Arc<dyn ServiceRegistry>,
    branch_buffer_factor: usize,
}

impl FederatedGraph {
    /// Create a new federated graph fetcher.
    /// query_planner is behind an arc futures::mutex for the following reasons:
    /// 1. query planning is potentially expensive, using a mutex will help to prevent denial of
    /// service attacks by serializing request that use new queries.
    /// 2. query planners may be mutable for caching state.
    /// 3. we can clone FederatedGraph for use across threads so we can make use of syntax.
    ///
    /// Subgraph registry is shared between all threads, but is expected to be Send and Sync and
    /// therefore can be used without obtaining a lock.
    pub fn new(
        query_planner: Arc<Mutex<dyn QueryPlanner>>,
        subgraph_registry: Arc<dyn ServiceRegistry>,
    ) -> FederatedGraph {
        FederatedGraph {
            branch_buffer_factor: 1,
            query_planner,
            service_registry: subgraph_registry,
        }
    }

    async fn plan(self, request: GraphQLRequest) -> Result<QueryPlan, FetchError> {
        log::debug!("Planning");
        let mut query_planner = self.query_planner.lock().await;
        let query_plan = query_planner.get(
            request.query.to_owned(),
            request.operation_name.to_owned(),
            QueryPlanOptions::default(),
        )?;

        if let Some(root) = &query_plan.node {
            //Check that all fetches are pointing to known services.
            self.validate_services(root)?;
        }

        Ok(query_plan)
    }

    fn validate_services(&self, node: &PlanNode) -> Result<(), FetchError> {
        match node {
            PlanNode::Parallel { nodes } => nodes
                .iter()
                .try_for_each(|node| self.validate_services(node))?,
            PlanNode::Sequence { nodes } => nodes
                .iter()
                .try_for_each(|node| self.validate_services(node))?,
            PlanNode::Flatten(flatten) => self.validate_services(flatten.node.as_ref())?,
            PlanNode::Fetch(fetch)
                if { !self.service_registry.has(fetch.service_name.to_owned()) } =>
            {
                return Err(FetchError::UnknownServiceError {
                    service: fetch.service_name.to_owned(),
                });
            }
            PlanNode::Fetch(_) => {}
        }

        Ok(())
    }

    fn visit(self, traversers: TraverserStream, node: PlanNode) -> TraverserStream {
        match node {
            PlanNode::Sequence { nodes } => self.visit_sequence(traversers, nodes),
            PlanNode::Parallel { nodes } => self.visit_parallel(traversers, nodes),
            PlanNode::Fetch(fetch) if fetch.requires.is_none() => {
                self.visit_fetch_no_select(traversers, fetch)
            }
            PlanNode::Fetch(fetch) => self.visit_fetch_select(traversers, fetch),
            PlanNode::Flatten(flatten) => self.visit_flatten(traversers, flatten.to_owned()),
        }
    }

    /// Perform a fetch where all the traversers available so far are collected and send as a batch
    ///
    fn visit_fetch_select(self, traversers: TraverserStream, fetch: FetchNode) -> TraverserStream {
        log::debug!("Fetch {:#?}", fetch.service_name);
        //TODO variable propagation

        let t = traversers
            .collect::<Vec<Traverser>>()
            .map(move |traversers| {
                let service_name = fetch.service_name.to_owned();
                // We already checked that the service exists duriong planning
                let fetcher = self.service_registry.get(service_name.clone()).unwrap();

                //First collect all the traversers and their selections
                //Filter out anything that didn't match a selection.
                let (traversers, selections): (Vec<_>, Vec<_>) = traversers
                    .iter()
                    .map(|traverser| (traverser.to_owned(), traverser.select(&fetch.requires)))
                    .filter(|(_, selection)| selection.is_some())
                    .unzip();

                let mut variables = Map::new();
                variables.insert(
                    "representations".into(),
                    Value::Array(
                        selections
                            .iter()
                            .map(|value| value.as_ref().unwrap().to_owned())
                            .collect(),
                    ),
                );
                log::debug!("Variables {}", to_string_pretty(&variables).unwrap());

                fetcher
                    .stream(GraphQLRequest {
                        query: fetch.operation.to_owned(),
                        operation_name: None,
                        variables: Some(variables),
                        extensions: None,
                    })
                    .into_future()
                    .map(move |(primary, _rest)| match primary {
                        // If we got results we zip the stream up with the original traverser and merge the results.
                        Some(Ok(GraphQLResponse::Primary(primary))) => {
                            log::debug!("Got {}", to_string_pretty(&primary.data).unwrap());
                            iter(
                                traversers
                                    .iter()
                                    .zip(
                                        primary
                                            .data
                                            .get("_entities")
                                            .unwrap()
                                            .as_array()
                                            .unwrap()
                                            .iter(),
                                    )
                                    .map(|(traverser, result)| {
                                        log::debug!(
                                            "Merging {} into {}",
                                            to_string_pretty(result).unwrap(),
                                            traverser.path()
                                        );
                                        traverser.to_owned().merge(Some(result))
                                    })
                                    .collect::<Vec<Traverser>>(),
                            )
                            .boxed()
                        }
                        Some(Ok(GraphQLResponse::Patch(_))) => {
                            panic!("Should not have had patch response as primary!")
                        }
                        Some(Err(err)) => iter(
                            traversers
                                .iter()
                                .map(|traverser| traverser.to_owned().add_err(&err))
                                .collect::<Vec<Traverser>>(),
                        )
                        .boxed(),
                        _ => iter(
                            traversers
                                .iter()
                                .map(|traverser| {
                                    traverser.to_owned().add_err(&FetchError::NoResponseError {
                                        service: service_name.to_owned(),
                                    })
                                })
                                .collect::<Vec<Traverser>>(),
                        )
                        .boxed(),
                    })
                    .boxed()
            })
            .flatten()
            .into_stream()
            .flatten()
            .boxed();
        t
    }

    /// Do a basic fetch with no selection.
    /// Traversers cannot be processed as a batch so we have make multiple queries.
    fn visit_fetch_no_select(
        self,
        traversers: TraverserStream,
        fetch: FetchNode,
    ) -> TraverserStream {
        log::debug!("Fetch without select {:#?}", fetch.service_name);
        //TODO variable propagation
        let branch_buffer_factor = self.branch_buffer_factor;
        traversers
            .map(move |traverser| {
                let service_name = fetch.service_name.to_owned();
                // We already validated that the service exists during planning
                let fetcher = self.service_registry.get(service_name.clone()).unwrap();
                fetcher
                    .stream(GraphQLRequest {
                        query: fetch.operation.to_owned(),
                        operation_name: None,
                        variables: None,
                        extensions: None,
                    })
                    .into_future()
                    .map(move |(primary, _rest)| match primary {
                        Some(Ok(GraphQLResponse::Primary(primary))) => {
                            log::debug!("Got {}", to_string_pretty(&primary.data).unwrap());
                            traverser.merge(Some(&Value::Object(primary.data)))
                        }
                        Some(Ok(GraphQLResponse::Patch(_))) => {
                            panic!("Should not have had patch response as primary!")
                        }
                        Some(Err(err)) => traverser.add_err(&err),
                        None => traverser.add_err(&FetchError::NoResponseError {
                            service: service_name,
                        }),
                    })
                    .boxed()
            })
            .buffered(branch_buffer_factor)
            .boxed()
    }

    /// Apply visit plan nodes in order, merging the results after each visit.
    fn visit_sequence(self, traversers: TraverserStream, nodes: Vec<PlanNode>) -> TraverserStream {
        log::debug!("Sequence");

        nodes
            .iter()
            .fold(traversers, move |acc, next| {
                self.to_owned().visit(acc, next.to_owned())
            })
            .boxed()
    }

    /// Take a stream query plan nodes and visit them in parallel
    /// This actually has the effect of stalling the pipeline until all traversers are collected.
    fn visit_parallel(self, traversers: TraverserStream, nodes: Vec<PlanNode>) -> TraverserStream {
        log::debug!("Parallel");
        nodes
            .iter()
            .fold(traversers, move |acc, next| {
                self.to_owned().visit(acc, next.to_owned())
            })
            .boxed()
        // traversers
        //     .collect::<Vec<Traverser>>()
        //     .into_stream()
        //     .flat_map(move |traversers| {
        //         let owned_s = self.to_owned();
        //         let streams = nodes
        //             .iter()
        //             .map(move |node| {
        //                 owned_s
        //                     .to_owned()
        //                     .visit(iter(traversers.to_owned()).boxed(), node.to_owned())
        //             })
        //             .collect::<Vec<_>>();
        //         select_all(streams).boxed()
        //     })
        //     .boxed()
    }

    /// Take a stream of nodes at a path in the currently fetched data and visit them with
    /// the query plan contained in the flatten node merging the results as the come back.
    fn visit_flatten(&self, traversers: TraverserStream, flatten: FlattenNode) -> TraverserStream {
        log::debug!("Flatten");
        let path = Path::parse(flatten.path.join("/"));
        let expanded = traversers
            .flat_map(move |traverser| traverser.stream_descendants(&path))
            .boxed();
        self.to_owned().visit(expanded, *flatten.node)
    }
}

impl GraphQLFetcher for FederatedGraph {
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream {
        let clone = self.to_owned();
        self.to_owned()
            .plan(request.to_owned())
            .map_ok(move |plan| match plan.node {
                Some(root) => clone
                    .visit(iter(vec![Traverser::new(request)]).boxed(), root.to_owned())
                    .flat_map(|response| iter(vec![Ok(response.to_primary())]))
                    .boxed(),
                None => empty().boxed(),
            })
            .map_err(|err| iter(vec![Err(err)]).boxed())
            .into_stream()
            .flat_map(|result| match result {
                Ok(s) => s,
                Err(e) => e,
            })
            .boxed()
    }
}

impl From<QueryPlannerError> for FetchError {
    fn from(err: QueryPlannerError) -> Self {
        FetchError::RequestError {
            reason: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use log::LevelFilter;
    use serde_json::to_string_pretty;

    use configuration::Configuration;
    use query_planner::harmonizer::HarmonizerQueryPlanner;

    use crate::http_service_registry::HttpServiceRegistry;
    use crate::http_subgraph::HttpSubgraphFetcher;

    use super::*;
    use maplit::hashmap;
    use std::collections::hash_map::Entry;
    use std::collections::HashMap;

    fn init() {
        let _ = env_logger::builder()
            //.filter_level(LevelFilter::Debug)
            .filter("execution".into(), LevelFilter::Debug)
            .is_test(true)
            .try_init();
    }

    #[tokio::test]
    async fn basic_request() {
        init();
        assert_federated_response(
            r#"{ topProducts { name } }"#,
            hashmap! {
                "products".to_string()=>1,
            },
        )
        .await
    }

    #[tokio::test]
    async fn basic_composition() {
        init();
        assert_federated_response(
            r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#,
            hashmap! {
                "products".to_string()=>5,
                "reviews".to_string()=>3,
                "accounts".to_string()=>4
            },
        )
        .await
    }

    async fn assert_federated_response(request: &str, service_requests: HashMap<String, usize>) {
        let request = GraphQLRequest {
            query: request.into(),
            operation_name: None,
            variables: None,
            extensions: None,
        };
        let mut expected = query_node(request.clone());
        let (mut actual, registry) = query_rust(request.clone());
        log::debug!(
            "{}",
            to_string_pretty(&actual.next().await.unwrap().unwrap().primary()).unwrap()
        );
        log::debug!(
            "{}",
            to_string_pretty(&expected.next().await.unwrap().unwrap().primary()).unwrap()
        );
        assert_eq!(registry.totals(), service_requests);
    }

    fn query_node(request: GraphQLRequest) -> GraphQLResponseStream {
        let nodejs_impl =
            HttpSubgraphFetcher::new("federated".into(), "http://localhost:4000/graphql".into());
        nodejs_impl.stream(request)
    }

    fn query_rust(
        request: GraphQLRequest,
    ) -> (GraphQLResponseStream, Arc<CountingServiceRegistry>) {
        let planner =
            HarmonizerQueryPlanner::new(include_str!("testdata/supergraph.graphql").into());
        let config =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let registry = Arc::new(CountingServiceRegistry::new(HttpServiceRegistry::new(
            config,
        )));
        let federated = FederatedGraph::new(Arc::new(Mutex::new(planner)), registry.to_owned());
        (federated.stream(request), registry)
    }

    #[derive(Debug)]
    struct CountingServiceRegistry {
        counts: Arc<std::sync::Mutex<HashMap<String, usize>>>,
        delegate: HttpServiceRegistry,
    }

    impl CountingServiceRegistry {
        fn new(delegate: HttpServiceRegistry) -> CountingServiceRegistry {
            CountingServiceRegistry {
                counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
                delegate,
            }
        }

        fn totals(&self) -> HashMap<String, usize> {
            self.counts.lock().unwrap().clone()
        }
    }

    impl ServiceRegistry for CountingServiceRegistry {
        fn get(&self, service: String) -> Option<&dyn GraphQLFetcher> {
            let mut counts = self.counts.lock().unwrap();
            match counts.entry(service.to_owned()) {
                Entry::Occupied(mut e) => {
                    *e.get_mut() += 1;
                }
                Entry::Vacant(e) => {
                    e.insert(1);
                }
            }
            self.delegate.get(service)
        }

        fn has(&self, service: String) -> bool {
            self.delegate.has(service)
        }
    }
}
