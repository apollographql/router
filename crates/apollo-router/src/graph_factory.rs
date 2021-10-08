use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use apollo_router_core::prelude::*;
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::sync::Arc;

/// Factory for creating graphs.
///
/// This trait enables us to test that `StateMachine` correctly recreates the FederatedGraph when
/// necessary e.g. when schema changes.
#[cfg_attr(test, automock)]
pub(crate) trait GraphFactory<F>
where
    F: graphql::Fetcher,
{
    fn create(&self, configuration: &Configuration, schema: Arc<graphql::Schema>) -> F;
}

#[derive(Default)]
pub(crate) struct FederatedGraphFactory;

impl GraphFactory<graphql::FederatedGraph> for FederatedGraphFactory {
    fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
    ) -> graphql::FederatedGraph {
        let service_registry = HttpServiceRegistry::new(configuration);
        graphql::FederatedGraph::new(
            Box::new(graphql::HarmonizerQueryPlanner::new(&schema).with_caching()),
            Arc::new(service_registry),
            schema,
        )
    }
}
