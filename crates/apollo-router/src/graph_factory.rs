use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use apollo_router_core::{FederatedGraph, GraphQLFetcher, HarmonizerQueryPlanner, WithCaching};
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::sync::Arc;

/// Factory for creating graphs
///
/// This trait enables us to test that `StateMachine` correctly recreates the FederatedGraph when
/// necessary e.g. when schema changes.
#[cfg_attr(test, automock)]
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
