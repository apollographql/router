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
        query_cache_limit: usize,
    ) -> future::BoxFuture<'static, Router>;
    fn recreate(
        &self,
        graph: Arc<Router>,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        query_cache_limit: usize,
    ) -> future::BoxFuture<'static, Router>;
    fn get_query_cache_limit(&self) -> usize;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory {
    query_cache_limit: usize,
}
impl ApolloRouterFactory {
    pub fn new(query_cache_limit: usize) -> Self {
        Self { query_cache_limit }
    }
}

impl RouterFactory<ApolloRouter, ApolloPreparedQuery> for ApolloRouterFactory {
    fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        query_cache_limit: usize,
    ) -> future::BoxFuture<'static, ApolloRouter> {
        let service_registry = HttpServiceRegistry::new(configuration);
        tokio::task::spawn_blocking(move || {
            ApolloRouter::new(
                Arc::new(
                    graphql::RouterBridgeQueryPlanner::new(Arc::clone(&schema))
                        .with_caching(query_cache_limit),
                ),
                Arc::new(service_registry),
                schema,
            )
        })
        .map(|res| res.expect("ApolloRouter::new() is infallible; qed"))
        .boxed()
    }

    fn recreate(
        &self,
        graph: Arc<ApolloRouter>,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        query_cache_limit: usize,
    ) -> future::BoxFuture<'static, ApolloRouter> {
        let factory = self.create(configuration, schema, query_cache_limit);

        tokio::task::spawn(async move {
            // Use the "hot" entries in the supplied graph to pre-populate
            // our new graph
            let new_graph = factory.await;
            let hot_keys = graph.get_query_planner().get_hot_keys().await;
            // It would be nice to get these keys concurrently by spawning
            // futures in our loop. However, these calls to get call the
            // v8 based query planner and running too many of these
            // concurrently is a bad idea. One for the future...
            for key in hot_keys {
                // We can ignore errors, since we are just warming up the
                // cache
                let _ = new_graph.get_query_planner().get(key.0, key.1, key.2).await;
            }
            new_graph
        })
        .map(|res| res.expect("recreate() is infallible; qed"))
        .boxed()
    }

    fn get_query_cache_limit(&self) -> usize {
        self.query_cache_limit
    }
}
