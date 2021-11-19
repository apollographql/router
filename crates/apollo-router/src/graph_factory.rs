use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use apollo_router_core::prelude::{graphql::*, *};
use async_trait::async_trait;
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::sync::Arc;

/// Factory for creating graphs.
///
/// This trait enables us to test that `StateMachine` correctly recreates the FederatedGraph when
/// necessary e.g. when schema changes.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait GraphFactory<F, R>
where
    F: graphql::Router<R>,
    R: graphql::Route,
{
    async fn create(&self, configuration: &Configuration, schema: Arc<graphql::Schema>) -> F;
}

#[derive(Default)]
pub(crate) struct FederatedGraphFactory;

#[async_trait]
impl GraphFactory<graphql::FederatedGraph, graphql::FederatedGraphRoute> for FederatedGraphFactory {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
    ) -> graphql::FederatedGraph {
        let service_registry = HttpServiceRegistry::new(configuration);
        tokio::task::spawn_blocking(|| {
            graphql::FederatedGraph::new(
                Arc::new(
                    graphql::RouterBridgeQueryPlanner::new(Arc::clone(&schema)).with_caching(),
                ),
                Arc::new(service_registry),
                schema,
            )
        })
        .await
        .expect("FederatedGraph::new() is infallible; qed")
    }
}
