use crate::configuration::Configuration;
use apollo_router_core::prelude::*;
use apollo_router_core::{
    Context, PluggableRouterServiceBuilder, Plugin, RouterRequest, RouterResponse, Schema,
    SubgraphRequest,
};
use http::{Request, Response};
use static_assertions::assert_impl_all;
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::BoxCloneService;
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_service::Service;
use typed_builder::TypedBuilder;

/// Factory for creating graphs.
///
/// This trait enables us to test that `StateMachine` correctly recreates the ApolloRouter when
/// necessary e.g. when schema changes.
pub trait RouterFactory: Send + Sync + 'static {
    type RouterService: Service<Request<graphql::Request>, Response = Response<graphql::Response>, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static;

    fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<Self::RouterService>,
    ) -> Self::RouterService;
}

assert_impl_all!(ApolloRouterFactory: Send);
#[derive(Default, TypedBuilder)]
pub struct ApolloRouterFactory {
    plugins: Vec<Box<dyn Plugin>>,
    services: Vec<(
        String,
        Buffer<BoxCloneService<SubgraphRequest, RouterResponse, BoxError>, SubgraphRequest>,
    )>,
}

impl RouterFactory for ApolloRouterFactory {
    type RouterService = Buffer<
        BoxCloneService<Request<graphql::Request>, Response<graphql::Response>, BoxError>,
        Request<graphql::Request>,
    >;
    fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<Schema>,
        previous_router: Option<Self::RouterService>,
    ) -> Self::RouterService {
        //TODO Use the plugins, services and config tp build the pipeline.

        let buffer = 20000;
        ServiceBuilder::new().buffer(buffer).service(
            PluggableRouterServiceBuilder::new(schema, buffer)
                .build()
                .map_request(|http_request| RouterRequest {
                    http_request,
                    context: Context::new(),
                })
                .map_response(|response| response.response)
                .boxed_clone(),
        )
    }
}
