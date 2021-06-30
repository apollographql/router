use std::pin::Pin;
use std::sync::Arc;

use futures::future::ready;
use futures::lock::Mutex;
use futures::stream::{empty, iter};
use futures::{Future, FutureExt, StreamExt, TryFutureExt};
use serde_json::{Map, Value};

use query_planner::model::{FetchNode, FlattenNode, PlanNode, QueryPlan};
use query_planner::{QueryPlanOptions, QueryPlanner, QueryPlannerError};

use crate::traverser::Traverser;
use crate::{
    FetchError, GraphQLError, GraphQLFetcher, GraphQLPrimaryResponse, GraphQLRequest,
    GraphQLResponse, GraphQLResponseStream, SubgraphRegistry,
};

type TraversalResponseFuture = Pin<Box<dyn Future<Output = TraversalResponse> + Send>>;

/// Each traversal response contains contains some json content and a path that defines where the content came from.
/// Unlike the request this does not need to be clonable as we never have to flatmap.
struct TraversalResponse {
    traverser: Traverser,
    #[allow(dead_code)]
    patches: Vec<GraphQLResponseStream>,
    #[allow(dead_code)]
    errors: Vec<GraphQLError>,
}

impl TraversalResponse {
    fn to_primary(&self) -> GraphQLResponse {
        GraphQLResponse::Primary(GraphQLPrimaryResponse {
            data: match self.traverser.content.to_owned() {
                Some(Value::Object(obj)) => obj,
                _ => Map::new(),
            },
            has_next: None,
            errors: None,
            extensions: None,
        })
    }
}

/// Federated graph fetcher creates a query plan and executes the plan against one or more
/// subgraphs.
#[derive(Clone, Debug)]
pub struct FederatedGraph {
    query_planner: Arc<Mutex<dyn QueryPlanner>>,
    subgraph_registry: Arc<dyn SubgraphRegistry>,
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
        subgraph_registry: Arc<dyn SubgraphRegistry>,
    ) -> FederatedGraph {
        FederatedGraph {
            branch_buffer_factor: 1,
            query_planner,
            subgraph_registry,
        }
    }

    async fn plan(self, request: GraphQLRequest) -> Result<QueryPlan, FetchError> {
        let mut query_planner = self.query_planner.lock().await;
        let query_plan = query_planner.get(
            request.query.to_owned(),
            request.operation_name.to_owned(),
            QueryPlanOptions::default(),
        )?;
        log::debug!(
            "Query plan: {}",
            serde_json::to_string_pretty(&query_plan).unwrap()
        );
        Ok(query_plan)
    }

    fn visit(self, traverser: Traverser, node: PlanNode) -> TraversalResponseFuture {
        match node {
            PlanNode::Sequence { nodes } => self.visit_sequence(traverser, nodes),
            PlanNode::Parallel { nodes } => self.visit_parallel(traverser, nodes),
            PlanNode::Fetch(fetch) => self.visit_fetch(traverser, fetch),
            PlanNode::Flatten(flatten) => self.visit_flatten(traverser, flatten),
        }
    }

    fn visit_fetch(self, traverser: Traverser, fetch: FetchNode) -> TraversalResponseFuture {
        log::debug!("Visiting fetch {:#?}", fetch);
        //TODO variable propagation
        let service_name = fetch.service_name;
        let service = self.subgraph_registry.get(service_name.clone());
        match service {
            Some(fetcher) => {
                let mut variables = Map::new();

                match traverser.select(fetch.requires) {
                    Ok(Some(value)) => {
                        variables.insert("$representation".into(), value);
                    }
                    Err(err) => return ready(traverser.err(err)).boxed(),
                    _ => {}
                }

                fetcher
                    .stream(GraphQLRequest {
                        query: fetch.operation,
                        operation_name: None,
                        variables: Some(variables),
                        extensions: None,
                    })
                    .into_future()
                    .map(|(primary, rest)| match primary {
                        Some(Ok(GraphQLResponse::Primary(primary))) => TraversalResponse {
                            traverser,
                            patches: vec![rest],
                            errors: primary.errors.unwrap_or_default(),
                        },
                        Some(Ok(GraphQLResponse::Patch(_))) => {
                            panic!("Should not have had patch response as primary!")
                        }
                        Some(Err(err)) => traverser.err(err),
                        None => traverser.err(FetchError::NoResponseError {
                            service: service_name,
                        }),
                    })
                    .boxed()
            }
            None => ready(traverser.err(FetchError::UnknownServiceError {
                service: service_name,
            }))
            .boxed(),
        }
    }

    /// Apply visit plan nodes in order, merging the results after each visit.
    fn visit_sequence(self, traverser: Traverser, nodes: Vec<PlanNode>) -> TraversalResponseFuture {
        log::debug!("Visiting sequence of {:#?}", nodes);
        let response_traverser = traverser;

        iter(nodes)
            .fold(response_traverser.to_response(), move |acc, next| {
                self.to_owned()
                    .visit(acc.traverser.to_owned(), next)
                    .map(move |_response|
                            //TODO Do Merge!
                            acc)
            })
            .boxed()
    }

    /// Take a stream query plan nodes and visit them in parallel with the current traverser merging
    /// the results as they come back.
    fn visit_parallel(self, traverser: Traverser, nodes: Vec<PlanNode>) -> TraversalResponseFuture {
        log::debug!("Visiting parallel of {:#?}", nodes);
        let branch_buffer_factor = self.branch_buffer_factor;
        let response_traverser = traverser.clone();
        iter(nodes)
            .map(move |node| self.to_owned().visit(traverser.to_owned(), node))
            .buffer_unordered(branch_buffer_factor)
            .fold(response_traverser.to_response(), |acc, _next| async move {
                //TODO Do Merge!
                acc
            })
            .boxed()
    }

    /// Take a stream of nodes at a path in the currently fetched data and visit them with
    /// the query plan contained in the flatten node merging the results as the come back.
    fn visit_flatten(self, traverser: Traverser, flatten: FlattenNode) -> TraversalResponseFuture {
        log::debug!("Visiting flatten {:#?}", flatten);
        let branch_buffer_factor = self.branch_buffer_factor;
        let descendants = traverser.stream(flatten.path.iter().map(|a| a.into()).collect());

        descendants
            .map(move |descendant| self.to_owned().visit(descendant, *flatten.node.clone()))
            .buffer_unordered(branch_buffer_factor)
            .fold(traverser.to_response(), |acc, _next| async move {
                //TODO Do Merge!
                acc
            })
            .boxed()
    }
}

impl Traverser {
    fn err(self, err: FetchError) -> TraversalResponse {
        TraversalResponse {
            traverser: self.to_owned(),
            patches: vec![],
            errors: vec![GraphQLError {
                message: err.to_string(),
                locations: vec![],
                path: self.path,
                extensions: None,
            }],
        }
    }

    fn to_response(&self) -> TraversalResponse {
        TraversalResponse {
            traverser: self.clone(),
            patches: vec![],
            errors: vec![],
        }
    }
}

impl GraphQLFetcher for FederatedGraph {
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream {
        let clone = self.to_owned();
        self.to_owned()
            .plan(request.to_owned())
            .map_ok(move |plan| match plan.node {
                Some(root) => clone
                    .visit(Traverser::new(Arc::new(request)), root)
                    .into_stream()
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

    fn init() {
        let _ = env_logger::builder()
            .filter_level(LevelFilter::Debug)
            .is_test(true)
            .try_init();
    }

    #[tokio::test]
    async fn basic_composition() {
        init();
        assert_federated_response(
            r#"{
  topProducts {
    upc
    name

    reviews {
      id
      product{
        name
      }
      author {
        id
        name
      }
    }
  }
}
"#,
        )
        .await
    }

    async fn assert_federated_response(request: &str) {
        let request = GraphQLRequest {
            query: request.into(),
            operation_name: None,
            variables: None,
            extensions: None,
        };
        let mut expected = query_node(request.clone());
        let mut actual = query_rust(request.clone());
        assert_eq!(
            to_string_pretty(&expected.next().await.unwrap().unwrap().primary()).unwrap(),
            to_string_pretty(&actual.next().await.unwrap().unwrap().primary()).unwrap()
        );
    }

    fn query_node(request: GraphQLRequest) -> GraphQLResponseStream {
        let nodejs_impl =
            HttpSubgraphFetcher::new("federated".into(), "http://localhost:4000/graphql".into());
        nodejs_impl.stream(request)
    }

    fn query_rust(request: GraphQLRequest) -> GraphQLResponseStream {
        let planner =
            HarmonizerQueryPlanner::new(include_str!("testdata/supergraph.graphql").into());
        let config =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let registry = HttpServiceRegistry::new(config);
        let federated = FederatedGraph::new(Arc::new(Mutex::new(planner)), Arc::new(registry));
        federated.stream(request)
    }
}
