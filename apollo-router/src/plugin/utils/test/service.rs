use futures::stream::BoxStream;

use crate::{
    ExecutionRequest, ExecutionResponse, QueryPlannerRequest, QueryPlannerResponse, Response,
    ResponseBody, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
};

/// Build a mock service handler for the router pipeline.
#[macro_export]
macro_rules! mock_service {
    ($name:ident, $request_type:ty, $response_type:ty) => {
        paste::item! {
            #[mockall::automock]
            #[allow(dead_code)]
            pub trait [<$name Service>] {
                fn call(&self, req: $request_type) -> Result<$response_type, tower::BoxError>;
            }

            impl [<Mock $name Service>] {
                pub fn build(self) -> tower_test::mock::Mock<$request_type,$response_type> {
                    let (service, mut handle) = tower_test::mock::spawn();

                    tokio::spawn(async move {
                        loop {
                            while let Some((request, responder)) = handle.next_request().await {
                                match self.call(request) {
                                    Ok(response) => responder.send_response(response),
                                    Err(err) => responder.send_error(err),
                                }
                            }
                        }
                    });

                    service.into_inner()
                }
            }
        }
    };
}

mock_service!(
    Router,
    RouterRequest,
    RouterResponse<BoxStream<'static, ResponseBody>>
);
mock_service!(QueryPlanning, QueryPlannerRequest, QueryPlannerResponse);
mock_service!(
    Execution,
    ExecutionRequest,
    ExecutionResponse<BoxStream<'static, Response>>
);
mock_service!(Subgraph, SubgraphRequest, SubgraphResponse);
