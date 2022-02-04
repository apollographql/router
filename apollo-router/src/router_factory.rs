use crate::configuration::Configuration;
use crate::reqwest_subgraph_service::ReqwestSubgraphService;
use apollo_router_core::header_manipulation::HeaderManipulationLayer;
use apollo_router_core::prelude::*;
use apollo_router_core::{
    Context, PluggableRouterServiceBuilder, Plugin, RouterRequest, RouterResponse, Schema,
    SubgraphRequest,
};
use http::header::HeaderName;
use http::{Request, Response};
use static_assertions::assert_impl_all;
use std::str::FromStr;
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxLayer, BoxService};
use tower::{BoxError, Layer, ServiceBuilder, ServiceExt};
use tower_service::Service;
use tracing::instrument::WithSubscriber;
use typed_builder::TypedBuilder;

/// Factory for creating a RouterService
///
/// Instances of this traits are used by the StateMachine to generate a new
/// RouterService from configuration when it changes
#[async_trait::async_trait]
pub trait RouterServiceFactory: Send + Sync + 'static {
    type RouterService: Service<
            Request<graphql::Request>,
            Response = Response<graphql::Response>,
            Error = BoxError,
            Future = Self::Future,
        > + Send
        + Sync
        + Clone
        + 'static;
    type Future: Send;

    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<Self::RouterService>,
    ) -> Self::RouterService;
}

assert_impl_all!(ApolloRouterFactory: Send);
/// Main implementation of the RouterService factory, supporting the extensions system
#[derive(Default, TypedBuilder)]
pub struct ApolloRouterFactory {
    #[builder(default)]
    plugins: Vec<Box<dyn Plugin>>,
    #[builder(default)]
    services: Vec<(
        String,
        Buffer<BoxCloneService<SubgraphRequest, RouterResponse, BoxError>, SubgraphRequest>,
    )>,
}

#[async_trait::async_trait]
impl RouterServiceFactory for ApolloRouterFactory {
    type RouterService = Buffer<
        BoxCloneService<Request<graphql::Request>, Response<graphql::Response>, BoxError>,
        Request<graphql::Request>,
    >;
    type Future = <Self::RouterService as Service<Request<graphql::Request>>>::Future;

    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<Schema>,
        _previous_router: Option<Self::RouterService>,
    ) -> Self::RouterService {
        let dispatcher = configuration
            .subscriber
            .clone()
            .map(tracing::Dispatch::new)
            .unwrap_or_default();
        let buffer = 20000;
        //TODO Use the plugins, services and config tp build the pipeline.
        let mut builder = PluggableRouterServiceBuilder::new(schema, buffer, dispatcher.clone());

        if self.services.is_empty() {
            for (name, subgraph) in &configuration.subgraphs {
                let mut subgraph_service = BoxService::new(ReqwestSubgraphService::new(
                    name.to_string(),
                    subgraph.routing_url.clone(),
                ));

                for extension in &subgraph.extensions {
                    match extension.get("kind").as_ref().and_then(|v| v.as_str()) {
                        Some("header") => {
                            if let Some(header_name) =
                                extension.get("propagate").as_ref().and_then(|v| v.as_str())
                            {
                                subgraph_service =
                                    BoxLayer::new(HeaderManipulationLayer::propagate(
                                        HeaderName::from_str(header_name).unwrap(),
                                    ))
                                    .layer(subgraph_service);
                            }
                        }
                        _ => { //FIXME
                        }
                    }
                }

                builder = builder.with_subgraph_service(name, subgraph_service);
            }
        } else {
            for (name, subgraph) in &self.services {
                builder = builder.with_subgraph_service(name, subgraph.clone());
            }
        }

        let (service, worker) = Buffer::pair(
            ServiceBuilder::new().service(
                builder
                    .build()
                    .await
                    .map_request(|http_request| RouterRequest {
                        http_request,
                        context: Context::new(),
                    })
                    .map_response(|response| response.response)
                    .boxed_clone(),
            ),
            buffer,
        );
        tokio::spawn(worker.with_subscriber(dispatcher));
        service
    }
}
