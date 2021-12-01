use crate::apollo_router::{ApolloPreparedQuery, ApolloRouter};
use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use apollo_router_core::prelude::*;
use futures::prelude::*;
use std::sync::Arc;

/// Factory for creating graphs.
///
/// This trait enables us to test that `StateMachine` correctly recreates the ApolloRouter when
/// necessary e.g. when schema changes.
//#[cfg_attr(test, automock)]
pub(crate) trait RouterFactory<Router, PreparedQuery>
where
    Router: graphql::Router<PreparedQuery>,
    PreparedQuery: graphql::PreparedQuery,
{
    fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<Arc<Router>>,
    ) -> future::BoxFuture<'static, Router>;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory {}

impl ApolloRouterFactory {
    pub fn new() -> Self {
        Self {}
    }
}

impl RouterFactory<ApolloRouter, ApolloPreparedQuery> for ApolloRouterFactory {
    fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<Arc<ApolloRouter>>,
    ) -> future::BoxFuture<'static, ApolloRouter> {
        let service_registry = HttpServiceRegistry::new(configuration);
        ApolloRouter::new(Arc::new(service_registry), schema, previous_router).boxed()
    }
}
