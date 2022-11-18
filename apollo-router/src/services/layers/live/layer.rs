// With regards to ELv2 licensing, this entire file is license key functionality

use std::ops::ControlFlow;

use crate::layers::async_checkpoint::AsyncCheckpointService;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::SupergraphRequest;
use crate::SupergraphResponse;
use futures::future::BoxFuture;
use tower::buffer::Buffer;
use tower::BoxError;
use tower::Layer;
use tower::Service;

/// [`Layer`] for APQ implementation.
#[derive(Clone)]
pub(crate) struct LiveLayer {}

impl LiveLayer {
    pub(crate) async fn new() -> Self {
        Self {}
    }

    pub(crate) async fn request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        handle_request(request).await
    }
}

impl<S> Layer<S> for LiveLayer
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError> + Send + 'static,
    <S as Service<SupergraphRequest>>::Future: Send + 'static,
{
    type Service = AsyncCheckpointService<
        Buffer<S, SupergraphRequest>,
        BoxFuture<
            'static,
            Result<
                ControlFlow<<S as Service<SupergraphRequest>>::Response, SupergraphRequest>,
                BoxError,
            >,
        >,
        SupergraphRequest,
    >;

    fn layer(&self, service: S) -> Self::Service {
        AsyncCheckpointService::new(
            move |request| {
                Box::pin(async move {
                    match handle_request(request).await {
                        Ok(request) => Ok(ControlFlow::Continue(request)),
                        Err(response) => Ok(ControlFlow::Break(response)),
                    }
                })
                    as BoxFuture<
                        'static,
                        Result<
                            ControlFlow<
                                <S as Service<SupergraphRequest>>::Response,
                                SupergraphRequest,
                            >,
                            BoxError,
                        >,
                    >
            },
            Buffer::new(service, DEFAULT_BUFFER_SIZE),
        )
    }
}

pub(crate) async fn handle_request(
    _request: SupergraphRequest,
) -> Result<SupergraphRequest, SupergraphResponse> {
    unimplemented!()
}
