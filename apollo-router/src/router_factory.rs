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
pub(crate) trait RouterFactory<Router, ExecutionService> {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<graphql::RouterService<Router, ExecutionService>>,
    ) -> graphql::RouterService<Router, ExecutionService>;
}

#[derive(Default)]
pub(crate) struct ApolloRouterFactory {}

#[async_trait::async_trait]
impl RouterFactory<ApolloRouter, graphql::ExecutionService> for ApolloRouterFactory {
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<graphql::RouterService<ApolloRouter, graphql::ExecutionService>>,
    ) -> graphql::RouterService<ApolloRouter, graphql::ExecutionService> {
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
