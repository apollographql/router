use crate::{PlannedRequest, RouterRequest, RouterResponse, SubgraphRequest};
use mockall::automock;
use tower::{BoxError, Service};

macro_rules! mock_service {
    ($name:ident, $request_type:ty, $response_type:ty) => {
        paste::item! {
            #[automock]
            pub trait [<$name Service>] {
                fn call(&self, req: $request_type) -> Result<$response_type, BoxError>;
            }

            impl [<Mock $name Service>] {
                pub fn build(self) -> impl Service<$request_type, Response = $response_type, Error = BoxError> {
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

mock_service!(Router, RouterRequest, RouterResponse);
mock_service!(QueryPlanning, RouterRequest, PlannedRequest);
mock_service!(Execution, PlannedRequest, RouterResponse);
mock_service!(Subgraph, SubgraphRequest, RouterResponse);
