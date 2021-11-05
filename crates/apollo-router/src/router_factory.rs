use crate::apollo_router::{ApolloPreparedQuery, ApolloRouter};
use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use apollo_router_core::prelude::{graphql::*, *};
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
    ) -> future::BoxFuture<'static, Router>;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory;

impl RouterFactory<ApolloRouter, ApolloPreparedQuery> for ApolloRouterFactory {
    fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
    ) -> future::BoxFuture<'static, ApolloRouter> {
        let service_registry = HttpServiceRegistry::new(configuration);
        let c = configuration.clone();
        tokio::task::spawn_blocking(move || {
            let extensions = c.load_wasm_modules().unwrap();
            ApolloRouter::new(
                Arc::new(
                    graphql::RouterBridgeQueryPlanner::new(Arc::clone(&schema)).with_caching(),
                ),
                Arc::new(service_registry),
                schema,
                extensions,
            )
        })
        .map(|res| res.expect("ApolloRouter::new() is infallible; qed"))
        .boxed()
    }
}
