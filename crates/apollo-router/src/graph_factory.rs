use crate::apollo_router::{ApolloRoute, ApolloRouter};
use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use apollo_router_core::prelude::{graphql::*, *};
use async_trait::async_trait;
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::sync::Arc;

/// Factory for creating graphs.
///
/// This trait enables us to test that `StateMachine` correctly recreates the ApolloRouter when
/// necessary e.g. when schema changes.
#[cfg_attr(test, automock)]
#[async_trait]
pub(crate) trait GraphFactory<Router, Route>
where
    Router: graphql::Router<Route>,
    Route: graphql::Route,
{
    async fn create(&self, configuration: &Configuration, schema: Arc<graphql::Schema>) -> Router;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory;

#[async_trait]
impl GraphFactory<ApolloRouter, ApolloRoute> for ApolloRouterFactory {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
    ) -> ApolloRouter {
        let service_registry = HttpServiceRegistry::new(configuration);
        tokio::task::spawn_blocking(|| {
            ApolloRouter::new(
                Arc::new(
                    graphql::RouterBridgeQueryPlanner::new(Arc::clone(&schema)).with_caching(),
                ),
                Arc::new(service_registry),
                schema,
            )
        })
        .await
        .expect("ApolloRouter::new() is infallible; qed")
    }
}
