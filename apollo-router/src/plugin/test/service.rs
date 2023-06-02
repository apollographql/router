#![allow(dead_code, unreachable_pub)]
#![allow(missing_docs)] // FIXME

use futures::Future;
use hyper::Body;
use hyper::Request as HyperRequest;
use hyper::Response as HyperResponse;

use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

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
mock_service!(Router, RouterRequest, RouterResponse);
mock_service!(Supergraph, SupergraphRequest, SupergraphResponse);
mock_service!(Execution, ExecutionRequest, ExecutionResponse);
mock_service!(Subgraph, SubgraphRequest, SubgraphResponse);
mock_async_service!(HttpClient, HyperRequest<Body>, HyperResponse<Body>);
