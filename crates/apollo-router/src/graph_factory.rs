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
pub(crate) trait GraphFactory<F>
where
    F: graphql::Fetcher,
{
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        query_cache_limit: usize,
    ) -> F;
    async fn recreate(
        &self,
        graph: Arc<F>,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        query_cache_limit: usize,
    ) -> F;

    fn get_query_cache_limit(&self) -> usize;
}

#[derive(Default)]
pub(crate) struct FederatedGraphFactory {
    query_cache_limit: usize,
}
impl FederatedGraphFactory {
    pub fn new(query_cache_limit: usize) -> Self {
        Self { query_cache_limit }
    }
}

#[async_trait]
impl GraphFactory<graphql::FederatedGraph> for FederatedGraphFactory {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        query_cache_limit: usize,
    ) -> graphql::FederatedGraph {
        let service_registry = HttpServiceRegistry::new(configuration);
        tokio::task::spawn_blocking(move || {
            graphql::FederatedGraph::new(
                Arc::new(
                    graphql::RouterBridgeQueryPlanner::new(Arc::clone(&schema))
                        .with_caching(query_cache_limit),
                ),
                Arc::new(service_registry),
                schema,
            )
        })
        .await
        .expect("FederatedGraph::new() is infallible; qed")
    }

    async fn recreate(
        &self,
        graph: Arc<graphql::FederatedGraph>,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        query_cache_limit: usize,
    ) -> graphql::FederatedGraph {
        // Use the "hot" entries in the supplied graph to pre-populate
        // our new graph
        let hot_keys = graph.get_query_planner().get_hot_keys().await;
        let new_graph = self.create(configuration, schema, query_cache_limit).await;
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
    }

    fn get_query_cache_limit(&self) -> usize {
        self.query_cache_limit
    }
}
