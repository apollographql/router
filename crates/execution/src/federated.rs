use std::sync::Arc;

use futures::lock::Mutex;
use futures::stream::{empty, iter};
use futures::{FutureExt, StreamExt, TryFutureExt};

use crate::{FetchError, GraphQLFetcher, GraphQLRequest, GraphQLResponseStream, SubgraphRegistry};
use query_planner::model::{FetchNode, FlattenNode, PlanNode, QueryPlan};
use query_planner::{QueryPlanOptions, QueryPlanner, QueryPlannerError};

#[derive(Clone)]
struct Context {}

/// Federated graph fetcher creates a query plan and executes the plan against one or more
/// subgraphs.
#[derive(Clone, Debug)]
pub struct FederatedGraph {
    query_planner: Arc<Mutex<dyn QueryPlanner>>,
    subgraph_registry: Arc<dyn SubgraphRegistry>,
    branch_buffer_factor: usize,
}

type Response = GraphQLResponseStream;

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
        log::debug!("Query plan: {:#?}", &query_plan);
        Ok(query_plan)
    }

    fn visit(self, context: Context, node: PlanNode) -> Response {
        match node {
            PlanNode::Sequence { nodes } => self.visit_sequence(context, nodes),
            PlanNode::Parallel { nodes } => self.visit_parallel(context, nodes),
            PlanNode::Fetch(fetch) => self.visit_fetch(context, fetch),
            PlanNode::Flatten(flatten) => self.visit_flatten(context, flatten),
        }
    }

    fn visit_sequence(self, context: Context, nodes: Vec<PlanNode>) -> Response {
        log::debug!("Visiting sequence of {:#?}", nodes);
        iter(nodes)
            .flat_map(move |node| self.to_owned().visit(context.clone(), node))
            .boxed()
    }

    fn visit_parallel(self, context: Context, nodes: Vec<PlanNode>) -> Response {
        log::debug!("Visiting parallel of {:#?}", nodes);
        let branch_factor = self.branch_buffer_factor;
        iter(nodes)
            .map(move |node| self.to_owned().visit(context.clone(), node).into_future())
            .buffered(branch_factor)
            .map(|(next, _rest)| match next {
                Some(next) => iter(vec![next]).boxed(),
                None => empty().boxed(),
            })
            .flatten()
            .boxed()
    }

    fn visit_fetch(self, _context: Context, fetch: FetchNode) -> Response {
        log::debug!("Visiting fetch {:#?}", fetch);
        let service_name = fetch.service_name;
        let service = self.subgraph_registry.get(service_name.clone());
        match service {
            Some(fetcher) => fetcher.stream(GraphQLRequest {
                query: fetch.operation,
                operation_name: None,
                variables: None,
                extensions: None,
            }),
            None => err_stream(FetchError::UnknownServiceError {
                service: service_name,
            })
            .boxed(),
        }
    }

    fn visit_flatten(self, _context: Context, flatten: FlattenNode) -> Response {
        log::debug!("Visiting flatten {:#?}", flatten);
        empty().boxed()
    }
}

fn err_stream(error: FetchError) -> GraphQLResponseStream {
    iter(vec![Err(error)]).boxed()
}

impl GraphQLFetcher for FederatedGraph {
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream {
        let clone = self.to_owned();
        self.to_owned()
            .plan(request)
            .map_ok(move |plan| match plan.node {
                Some(root) => clone.visit(Context {}, root),
                None => empty().boxed(),
            })
            .map_err(err_stream)
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
    use serde_json::to_string_pretty;

    use configuration::Configuration;
    use query_planner::harmonizer::HarmonizerQueryPlanner;

    use crate::http_service_registry::HttpServiceRegistry;
    use crate::http_subgraph::HttpSubgraphFetcher;

    use super::*;
    use log::LevelFilter;

    fn init() {
        let _ = env_logger::builder()
            .filter_level(LevelFilter::Debug)
            .is_test(true)
            .try_init();
    }

    #[tokio::test]
    async fn basic_composition() {
        init();
        assert_federated_response(r#"{topProducts {upc name reviews {id author {id name}}}}"#).await
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
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph-config.yaml"))
                .unwrap();
        let registry = HttpServiceRegistry::new(config);
        let federated = FederatedGraph::new(Arc::new(Mutex::new(planner)), Arc::new(registry));
        federated.stream(request)
    }
}
