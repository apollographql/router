use crate::apollo_router::ApolloRouter;
use crate::configuration::Configuration;
use crate::reqwest_subgraph_service::ReqwestSubgraphService;
use apollo_router_core::prelude::*;
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
        previous_router: Option<graphql::RouterService<Router>>,
    ) -> graphql::RouterService<Router>;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory {}

#[async_trait::async_trait]
impl RouterFactory<ApolloRouter> for ApolloRouterFactory {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<graphql::RouterService<ApolloRouter>>,
    ) -> graphql::RouterService<ApolloRouter> {
        let mut service_registry = graphql::ServiceRegistry::new();
        for (name, subgraph) in &configuration.subgraphs {
            let fetcher =
                ReqwestSubgraphService::new(name.to_owned(), subgraph.routing_url.to_owned());
            service_registry.insert(name, fetcher);
        }
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
