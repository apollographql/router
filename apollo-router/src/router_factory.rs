use crate::apollo_router::{ApolloPreparedQuery, ApolloRouter};
use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use apollo_router_core::prelude::*;
use std::sync::Arc;

/// Factory for creating graphs.
///
/// This trait enables us to test that `StateMachine` correctly recreates the ApolloRouter when
/// necessary e.g. when schema changes.
#[async_trait::async_trait]
pub(crate) trait RouterFactory<Router, PreparedQuery>
where
    Router: graphql::Router<PreparedQuery>,
    PreparedQuery: graphql::PreparedQuery,
{
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<Arc<Router>>,
    ) -> Router;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory {}

#[async_trait::async_trait]
impl RouterFactory<ApolloRouter, ApolloPreparedQuery> for ApolloRouterFactory {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<Arc<ApolloRouter>>,
    ) -> ApolloRouter {
        let service_registry = HttpServiceRegistry::new(configuration);
        ApolloRouter::new(Arc::new(service_registry), schema, previous_router).await
    }
}
