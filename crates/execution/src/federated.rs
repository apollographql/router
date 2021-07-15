use std::pin::Pin;
use std::sync::Arc;

use futures::future::{join_all, ready};
use futures::lock::Mutex;
use futures::stream::{empty, iter};
use futures::{Future, FutureExt, Stream, StreamExt};
use serde_json::{Map, Value};

use query_planner::model::{FetchNode, FlattenNode, PlanNode, QueryPlan, SelectionSet};
use query_planner::{QueryPlanOptions, QueryPlanner, QueryPlannerError};

use crate::traverser::Traverser;
use crate::{
    FetchError, GraphQLFetcher, GraphQLPrimaryResponse, GraphQLRequest, GraphQLResponse,
    GraphQLResponseStream, Path, ServiceRegistry,
};

type TraverserStream = Pin<Box<dyn Stream<Item = Traverser> + Send>>;
type EmptyFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Federated graph fetcher creates a query plan and executes the plan against one or more
/// subgraphs. For information on how the algorithm works refer to the README for this crate.
#[derive(Clone, Debug)]
pub struct FederatedGraph {
    query_planner: Arc<Mutex<dyn QueryPlanner>>,
    service_registry: Arc<dyn ServiceRegistry>,
    concurrency_factor: usize,
    chunk_size: usize,
}

impl FederatedGraph {
    /// Create a new federated graph fetcher.
    /// query_planner is shared between threads and requires a lock for planning:
    /// 1. query planners may be mutable for caching state.
    /// 2. we can clone FederatedGraph for use across threads so we can make use of syntax.
    ///
    /// service_registry is shared between threads, but is send and sync and therefore does not need
    /// a mutex.
    ///
    /// concurrency_factor and chunk_size are not exposed right now. Setting chunk_size to 1 has
    /// the effect of serializing the execution in a predictable order, which can be useful for
    /// debugging.
    ///
    /// In future we may allow concurrency_factor and chunk_size to be set explicitly to allow
    /// clients to avoid stalled execution at the cost of making more downstream calls.
    ///
    /// # Arguments
    ///
    /// * `query_planner`: The query planner to use to for planning.
    /// * `service_registry`: The registry of service name to fetcher.
    ///
    /// returns: FederatedGraph
    ///
    pub fn new(
        query_planner: Arc<Mutex<dyn QueryPlanner>>,
        service_registry: Arc<dyn ServiceRegistry>,
    ) -> Self {
        Self {
            concurrency_factor: 100000,
            chunk_size: 100000,
            query_planner,
            service_registry,
        }
    }

    /// Create a query plan via the query planner   
    ///
    /// # Arguments
    ///
    /// * `request`: The request to be planned.
    ///
    /// returns: Result<QueryPlan, FetchError>
    ///
    async fn plan(self, request: GraphQLRequest) -> Result<QueryPlan, FetchError> {
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

    /// Recursively validate a query plan node making sure that all services are known before we go
    /// for execution.
    /// This simplifies processing later as we can always guarantee that services are configured for
    /// the plan.  
    ///
    /// # Arguments
    ///
    /// * `node`: The query plan node to validate.
    ///
    /// returns: Result<(), FetchError>
    ///
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

    /// Visit a stream of traversers with a plan node.
    /// Dispatches the visit to the fetch, sequence, parallel, or flatten operations.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `node`: The query plan node.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    fn visit(self, traversers: TraverserStream, node: PlanNode) -> EmptyFuture {
        let concurrency_factor = self.concurrency_factor;
        traversers
            .chunks(self.chunk_size)
            .map(move |traversers| {
                let traverser_stream = iter(traversers).boxed();
                let clone = self.to_owned();
                match node.to_owned() {
                    PlanNode::Sequence { nodes } => clone.visit_sequence(traverser_stream, nodes),
                    PlanNode::Parallel { nodes } => clone.visit_parallel(traverser_stream, nodes),
                    PlanNode::Fetch(fetch) if fetch.requires.is_none() => {
                        clone.visit_fetch_no_select(traverser_stream, fetch)
                    }
                    PlanNode::Fetch(fetch) => clone.visit_fetch_select(traverser_stream, fetch),
                    PlanNode::Flatten(flatten) => clone.visit_flatten(traverser_stream, flatten),
                }
                .boxed()
            })
            .buffer_unordered(concurrency_factor)
            .for_each(|_| ready(()))
            .boxed()
    }

    /// Fetch where the plan node has a selection.
    /// Selection fetches are performed in bulk and the results are merged back into the originating
    /// traverser.
    ///
    /// For each traverser we try and obtain data from the content that matches the selection.
    /// Any traversers that do not match anything are dropped.
    ///
    /// The selections are aggregated and sent to the downstream service, the result is merged back
    /// in with the originating traverser.  
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `fetch`: The fetch plan node.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    ///
    fn visit_fetch_select(self, traversers: TraverserStream, fetch: FetchNode) -> EmptyFuture {
        //TODO variable propagation

        traversers
            .collect::<Vec<Traverser>>()
            .map(move |traversers| {
                let service_name = fetch.service_name.to_owned();
                // We already checked that the service exists during planning
                let fetcher = self.service_registry.get(service_name.clone()).unwrap();
                let (traversers, selections) =
                    traversers_with_selections(&fetch.requires, traversers);

                let mut variables = Map::new();
                variables.insert(
                    "representations".into(),
                    construct_representations(selections),
                );

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
                            merge_results(&service_name, &traversers, primary);
                        }
                        Some(Ok(GraphQLResponse::Patch(_))) => {
                            panic!("Should not have had patch response as primary!")
                        }
                        Some(Err(err)) => {
                            traversers.iter().for_each(|t| t.add_err(&err));
                        }
                        _ => {
                            traversers.iter().for_each(|t| {
                                t.add_err(&FetchError::NoResponseError {
                                    service: service_name.to_owned(),
                                })
                            });
                        }
                    })
                    .boxed()
            })
            .flatten()
            .boxed()
    }

    /// Perform a fetch with no selections.
    /// Without selections the queries for each traverser must be made independently and cannot be
    /// batched.
    ///
    /// In practice non selection queries are likely to happen only at the top level of a query plan
    /// and will therefore only have one traverser.
    ///
    /// If a non-selection query does happen at a lower level with multiple traversers the requests
    /// happen in parallel.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The traversers to process.
    /// * `fetch`: The fetch node.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    ///
    fn visit_fetch_no_select(self, traversers: TraverserStream, fetch: FetchNode) -> EmptyFuture {
        //TODO variable propagation
        let concurrency_factor = self.concurrency_factor;
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
                            traverser.merge(Some(&Value::Object(primary.data)));
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
            .buffered(concurrency_factor)
            .for_each(|_| ready(()))
            .boxed()
    }

    /// Visit a sequence of plan nodes in turn.
    /// Execution waits for the previous operations to complete before executing the next operation
    /// in the query plan.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `nodes`: The plan nodes in the sequence.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    fn visit_sequence(self, traversers: TraverserStream, nodes: Vec<PlanNode>) -> EmptyFuture {
        traversers
            .collect::<Vec<Traverser>>()
            .map(move |traversers| {
                // We now have a chunk of traversers
                nodes
                    .iter()
                    .fold(ready(()).boxed(), |acc, node| {
                        let next = self
                            .to_owned()
                            .visit(iter(traversers.to_owned()).boxed(), node.to_owned())
                            .boxed();

                        acc.then(|_| next).boxed()
                    })
                    .boxed()
            })
            .flatten()
            .boxed()
    }

    /// Visit a set of plan nodes in parallel.
    /// Execution of all child operations happens in parallel, however the parallel operation cannot
    /// complete until all child operations have completed.
    ///
    /// With large chunk sizes there is the potential that a stalled operation will stall the
    /// entire pipeline.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `nodes`: The pan nodes to execute in parallel.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    fn visit_parallel(self, traversers: TraverserStream, nodes: Vec<PlanNode>) -> EmptyFuture {
        traversers
            .collect::<Vec<Traverser>>()
            .map(move |traversers| {
                // We now have a chunk of traversers
                // For each parallel branch we send clones of those traversers through the pipeline
                let tasks = nodes
                    .iter()
                    .map(move |node| {
                        self.to_owned()
                            .visit(iter(traversers.to_owned()).boxed(), node.to_owned())
                    })
                    .collect::<Vec<_>>();
                join_all(tasks).map(|_| ())
            })
            .flatten()
            .boxed()
    }

    /// Visit a flatten plan node.
    /// Given a traverser this will create a stream of traversers that match the path provided in
    /// the plan.
    ///
    /// For instance given:
    /// ```json
    /// {
    ///     'a': {
    ///         'b':[{'c':1}, {'c':2}]
    ///     }
    /// ```  
    /// a traverser at path `a`
    /// and a plan path of `b/@/c`
    /// The traversers generated will be:
    /// `a/b/0/c' and `a/b/1/c'
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `flatten`: The flatten plan node.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    ///
    fn visit_flatten(&self, traversers: TraverserStream, flatten: FlattenNode) -> EmptyFuture {
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
            .into_stream()
            .flat_map(move |plan| match plan {
                Ok(QueryPlan { node: Some(root) }) => {
                    let start = Traverser::new(request.to_owned());
                    clone
                        .to_owned()
                        .visit(iter(vec![start.to_owned()]).boxed(), root)
                        .map(move |_| iter(vec![Ok(start.to_primary())]))
                        .flatten_stream()
                        .boxed()
                }
                Ok(_) => empty().boxed(),
                Err(err) => iter(vec![Err(err)]).boxed(),
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

/// Given a vec of selections merge them into an array value.
///
/// # Arguments
///
/// * `selections`: The selections to merge.
///
/// returns: Value
///
fn construct_representations(selections: Vec<Value>) -> Value {
    Value::Array(selections.iter().map(|value| value.to_owned()).collect())
}

/// Get the list of traversers and corresponding selections for sending to a downstream service.
/// Any traverser that does not result in a selection will be dropped.
///
/// # Arguments
///
/// * `fetch`: The fetch node that defines the
/// * `traversers`: The vec of traversers to process.
///
/// returns: (Vec<Traverser>, Vec<Value>)
///
fn traversers_with_selections(
    requires: &Option<SelectionSet>,
    traversers: Vec<Traverser>,
) -> (Vec<Traverser>, Vec<Value>) {
    traversers
        .iter()
        .map(|traverser| (traverser.to_owned(), traverser.select(requires)))
        .filter(|(_, selection)| selection.is_some())
        .map(|(traverser, selection)| (traverser, selection.unwrap()))
        .unzip()
}

/// Merge the results of a selection query with the originating traverser.
/// Each result is paired with the originating traverser before merging.
///
/// # Arguments
///
/// * `traversers`: The vec of traversers to merge with.
/// * `primary`: The response from the downstream server
///
/// returns: Vec<Traverser>
///
fn merge_results(service: &str, traversers: &[Traverser], primary: GraphQLPrimaryResponse) {
    match primary.data.get("_entities") {
        Some(Value::Array(array)) => {
            traversers
                .iter()
                .zip(array.iter())
                .for_each(|(traverser, result)| {
                    traverser.to_owned().merge(Some(result));
                });
        }
        _ => traversers.iter().for_each(|traverser| {
            traverser.add_err(&FetchError::ServiceError {
                service: service.into(),
                reason: "Malformed response".to_string(),
            });
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::Entry;
    use std::collections::HashMap;

    use futures::StreamExt;
    use maplit::hashmap;
    use serde_json::to_string_pretty;

    use configuration::Configuration;
    use query_planner::harmonizer::HarmonizerQueryPlanner;

    use crate::http_service_registry::HttpServiceRegistry;
    use crate::http_subgraph::HttpSubgraphFetcher;
    use crate::json_utils::is_subset;

    use super::*;

    fn init() {
        let _ = env_logger::builder()
            //.filter_level(LevelFilter::Debug)
            //.filter("execution".into(), LevelFilter::Debug)
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
                "products".to_string()=>2,
                "reviews".to_string()=>1,
                "accounts".to_string()=>1
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

        let actual = actual.next().await.unwrap().unwrap().primary();
        let expected = expected.next().await.unwrap().unwrap().primary();
        log::debug!("{}", to_string_pretty(&actual).unwrap());
        log::debug!("{}", to_string_pretty(&expected).unwrap());

        // The current implementation does not cull extra properties that should not make is to the
        // output yet, so we check that the nodejs implementation returns a subset of the
        // output of the rust output.
        assert!(is_subset(
            &Value::Object(expected.data),
            &Value::Object(actual.data)
        ));
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
            &config,
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
