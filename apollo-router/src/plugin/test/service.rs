#![allow(dead_code, unreachable_pub)]
#![allow(missing_docs)] // FIXME

#[cfg(test)]
use std::sync::Arc;

use futures::Future;
use http::Request as HyperRequest;
use http::Response as HyperResponse;

use crate::services::router::Body;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
#[cfg(test)]
use crate::services::HasSchema;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
#[cfg(test)]
use crate::spec::Schema;

/// Build a mock service handler for the router pipeline.
macro_rules! mock_service {
    ($name:ident, $request_type:ty, $response_type:ty) => {
        paste::item! {
            mockall::mock! {
                #[derive(Debug)]
                #[allow(dead_code)]
                pub [<$name Service>] {
                    pub fn call(&mut self, req: $request_type) -> Result<$response_type, tower::BoxError>;
                }

                #[allow(dead_code)]
                impl Clone for [<$name Service>] {
                    fn clone(&self) -> [<Mock $name Service>];
                }
            }

            // mockall does not handle well the lifetime on Context
            impl tower::Service<$request_type> for [<Mock $name Service>] {
                type Response = $response_type;
                type Error = tower::BoxError;
                type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

                fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), tower::BoxError>> {
                    std::task::Poll::Ready(Ok(()))
                }
                fn call(&mut self, req: $request_type) -> Self::Future {
                    let r  = self.call(req);
                    Box::pin(async move { r })
                }
            }
        }
    };
}

macro_rules! mock_async_service {
    ($name:ident, $request_type:tt < $req_generic:tt > , $response_type:tt < $res_generic:tt >) => {
        paste::item! {
            mockall::mock! {
                #[derive(Debug)]
                #[allow(dead_code)]
                pub [<$name Service>] {
                    pub fn call(&mut self, req: $request_type<$req_generic>) -> impl Future<Output = Result<$response_type<$res_generic>, tower::BoxError>> + Send + 'static;
                }

                #[allow(dead_code)]
                impl Clone for [<$name Service>] {
                    fn clone(&self) -> [<Mock $name Service>];
                }
            }


            // mockall does not handle well the lifetime on Context
            impl tower::Service<$request_type<$req_generic>> for [<Mock $name Service>] {
                type Response = $response_type<$res_generic>;
                type Error = tower::BoxError;
                type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

                fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), tower::BoxError>> {
                    std::task::Poll::Ready(Ok(()))
                }
                fn call(&mut self, req: $request_type<$req_generic>) -> Self::Future {
                    Box::pin(self.call(req))
                }
            }
        }
    };
}

#[cfg(test)]
impl HasSchema for MockSupergraphService {
    fn schema(&self) -> Arc<crate::spec::Schema> {
        Arc::new(
            Schema::parse(
                include_str!("../../testdata/supergraph.graphql"),
                &Default::default(),
            )
            .unwrap(),
        )
    }
}

mock_service!(Router, RouterRequest, RouterResponse);
mock_service!(Supergraph, SupergraphRequest, SupergraphResponse);
mock_service!(Execution, ExecutionRequest, ExecutionResponse);
mock_service!(Subgraph, SubgraphRequest, SubgraphResponse);
mock_async_service!(HttpClient, HyperRequest<Body>, HyperResponse<Body>);

// This type is introduced to update internal uses of mocked http services, because the HttpClientService
// defined above is part of the public API
// TODO Router 2: remove this type and replace HttpClientService
#[cfg(test)]
pub(crate) use internal::MockInternalHttpClientService;
#[cfg(test)]
mod internal {
    use futures::Future;
    use http::Request as HyperRequest;
    use http::Response as HyperResponse;

    use crate::services::router::body::RouterBody;

    mock_async_service!(
        InternalHttpClient,
        HyperRequest<RouterBody>,
        HyperResponse<RouterBody>
    );
}
