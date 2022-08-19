#![allow(dead_code, unreachable_pub)]
#![allow(missing_docs)] // FIXME

use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::SubgraphRequest;
use crate::SubgraphResponse;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

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

mock_service!(Supergraph, SupergraphRequest, SupergraphResponse);
mock_service!(QueryPlanner, QueryPlannerRequest, QueryPlannerResponse);
mock_service!(Execution, ExecutionRequest, ExecutionResponse);
mock_service!(Subgraph, SubgraphRequest, SubgraphResponse);
