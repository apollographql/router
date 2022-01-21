use crate::apollo_router::ApolloRouter;
use crate::configuration::Configuration;
use crate::http_service_registry::HttpServiceRegistry;
use crate::http_subgraph::HttpSubgraphFetcher;
use apollo_router_core::{prelude::*, RouterService};
use std::sync::Arc;

/// Factory for creating graphs.
///
/// This trait enables us to test that `StateMachine` correctly recreates the ApolloRouter when
/// necessary e.g. when schema changes.
#[async_trait::async_trait]
pub(crate) trait RouterFactory<Router>
where
    Router: graphql::Router,
{
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<RouterService<Router>>,
    ) -> RouterService<Router>;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory {}

#[async_trait::async_trait]
impl RouterFactory<ApolloRouter> for ApolloRouterFactory {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<RouterService<ApolloRouter>>,
    ) -> RouterService<ApolloRouter> {
        let service_registry = graphql::ServiceRegistry2::new(configuration.subgraphs.iter().map(
            |(name, subgraph)| {
                let fetcher = Box::new(graphql::FetcherService::new(HttpSubgraphFetcher::new(
                    name.to_owned(),
                    subgraph.routing_url.to_owned(),
                ))) as Box<_>;
                (name.to_string(), fetcher)
            },
        ));
        graphql::RouterService::new(Arc::new(
            ApolloRouter::new(
                Arc::new(service_registry),
                schema,
                previous_router.map(|r| r.into_inner()),
            )
            .await,
        ))
    }
}
