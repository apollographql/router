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
    ) -> future::BoxFuture<'static, Router>;
    fn recreate(
        &self,
        router: Arc<Router>,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
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
    ) -> future::BoxFuture<'static, ApolloRouter> {
        let service_registry = HttpServiceRegistry::new(configuration);
        tokio::task::spawn_blocking(move || ApolloRouter::new(Arc::new(service_registry), schema))
            .map(|res| res.expect("ApolloRouter::new() is infallible; qed"))
            .boxed()
    }

    fn recreate(
        &self,
        router: Arc<ApolloRouter>,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
    ) -> future::BoxFuture<'static, ApolloRouter> {
        let factory = self.create(configuration, schema);

        Box::pin(async move {
            // Use the "hot" entries in the supplied router to pre-populate
            // our new router
            let new_router = factory.await;
            let hot_keys = router.get_query_planner().get_hot_keys().await;
            // It would be nice to get these keys concurrently by spawning
            // futures in our loop. However, these calls to get call the
            // v8 based query planner and running too many of these
            // concurrently is a bad idea. One for the future...
            for key in hot_keys {
                // We can ignore errors, since we are just warming up the
                // cache
                let _ = new_router
                    .get_query_planner()
                    .get(key.0, key.1, key.2)
                    .await;
            }
            new_router
        })
    }
}
