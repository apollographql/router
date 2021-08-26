use configuration::Configuration;
use execution::federated::FederatedGraph;
use execution::http_service_registry::HttpServiceRegistry;
use execution::GraphQLFetcher;
#[cfg(feature = "mocks")]
use mockall::{automock, predicate::*};
use query_planner::caching::WithCaching;
use query_planner::harmonizer::HarmonizerQueryPlanner;
use std::sync::Arc;

/// Factory for creating graphs
///
/// This trait enables us to test that `StateMachine` correctly recreates the FederatedGraph when
/// necessary e.g. when schema changes.
#[cfg_attr(feature = "mocks", automock)]
pub(crate) trait GraphFactory<F>
where
    F: GraphQLFetcher,
{
    fn create(&self, configuration: &Configuration, schema: &str) -> F;
}

#[derive(Default)]
pub(crate) struct FederatedGraphFactory;

impl GraphFactory<FederatedGraph> for FederatedGraphFactory {
    fn create(&self, configuration: &Configuration, schema: &str) -> FederatedGraph {
        let service_registry = HttpServiceRegistry::new(configuration);
        FederatedGraph::new(
            Box::new(HarmonizerQueryPlanner::new(schema.to_owned()).with_caching()),
            Arc::new(service_registry),
        )
    }
}
