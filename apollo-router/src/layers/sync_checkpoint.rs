//! Synchronous Checkpoint
//!
//! A crate-private counterpart to [`AsyncCheckpointService`](super::async_checkpoint::AsyncCheckpointService)
//! for checks that never need to `.await`.
//!
//! An async checkpoint has to move the inner service into its future, so it clones the inner
//! service on every call. `BoxCloneService::clone` is deep: it clones every boxed layer beneath it
//! down to the next `Buffer`, so each async checkpoint in a stack costs a clone of the whole
//! service tree under it, per request. A synchronous checkpoint runs the check in `call()` and, on
//! [`ControlFlow::Continue`], calls the inner service it was readied with directly, so it never
//! clones.
//!
//! Because the check runs in `call()` rather than when the returned future is polled, it runs
//! under whichever span is current at `call()` time.

use std::future::Ready;
use std::ops::ControlFlow;
use std::sync::Arc;

use futures::future::Either;
use tower::Layer;
use tower::Service;

type CheckpointFn<Request, Response, Error> =
    dyn Fn(Request) -> Result<ControlFlow<Response, Request>, Error> + Send + Sync;

/// [`Layer`] for synchronous checkpoints.
pub(crate) struct CheckpointLayer<S, Request>
where
    S: Service<Request>,
{
    checkpoint_fn: Arc<CheckpointFn<Request, S::Response, S::Error>>,
}

impl<S, Request> CheckpointLayer<S, Request>
where
    S: Service<Request>,
{
    /// Create a `CheckpointLayer` from a function that takes a service request and returns a
    /// `ControlFlow`: `Continue` with the request to pass it on, or `Break` with a response to
    /// return it without calling the inner service.
    pub(crate) fn new(
        checkpoint_fn: impl Fn(Request) -> Result<ControlFlow<S::Response, Request>, S::Error>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            checkpoint_fn: Arc::new(checkpoint_fn),
        }
    }
}

impl<S, Request> Layer<S> for CheckpointLayer<S, Request>
where
    S: Service<Request>,
{
    type Service = CheckpointService<S, Request>;

    fn layer(&self, inner: S) -> Self::Service {
        CheckpointService {
            inner,
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
        }
    }
}

/// [`Service`] for synchronous checkpoints. See [`CheckpointLayer`].
pub(crate) struct CheckpointService<S, Request>
where
    S: Service<Request>,
{
    inner: S,
    checkpoint_fn: Arc<CheckpointFn<Request, S::Response, S::Error>>,
}

impl<S, Request> Clone for CheckpointService<S, Request>
where
    S: Service<Request> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
        }
    }
}

impl<S, Request> Service<Request> for CheckpointService<S, Request>
where
    S: Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Either<Ready<Result<S::Response, S::Error>>, S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match (self.checkpoint_fn)(req) {
            Ok(ControlFlow::Continue(req)) => Either::Right(self.inner.call(req)),
            Ok(ControlFlow::Break(response)) => Either::Left(std::future::ready(Ok(response))),
            Err(error) => Either::Left(std::future::ready(Err(error))),
        }
    }
}

#[cfg(test)]
mod tests {
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use super::*;
    use crate::services::ExecutionRequest;
    use crate::services::ExecutionResponse;

    #[tokio::test]
    async fn test_continue() {
        let expected_label = "from_mock_service";
        let (mock, mut handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let label = expected_label.to_string();
        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                ExecutionResponse::fake_builder()
                    .label(label)
                    .build()
                    .unwrap(),
            );
        });

        let service_stack = ServiceBuilder::new()
            .layer(CheckpointLayer::new(|req| Ok(ControlFlow::Continue(req))))
            .service(mock);

        let actual_label = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .label
            .unwrap();

        assert_eq!(actual_label, expected_label);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_return() {
        let (mock, handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let service_stack = CheckpointLayer::new(|_req| {
            Ok(ControlFlow::Break(
                ExecutionResponse::fake_builder()
                    .label("returned_before_mock_service".to_string())
                    .build()
                    .unwrap(),
            ))
        })
        .layer(mock);

        let actual_label = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .label
            .unwrap();

        assert_eq!(actual_label, "returned_before_mock_service");
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_error() {
        let expected_error = "checkpoint_error";
        let (mock, handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let service_stack =
            CheckpointLayer::new(move |_req| Err(BoxError::from(expected_error))).layer(mock);

        let actual_error = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .map(|_| unreachable!())
            .unwrap_err()
            .to_string();

        assert_eq!(actual_error, expected_error);
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_double_call_doesnt_panic() {
        let (mock, handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let mut service_stack = CheckpointLayer::new(|_req| {
            Ok(ControlFlow::Break(
                ExecutionResponse::fake_builder()
                    .label("returned_before_mock_service".to_string())
                    .build()
                    .unwrap(),
            ))
        })
        .layer(mock);

        service_stack.ready().await.unwrap();
        service_stack
            .call(ExecutionRequest::fake_builder().build())
            .await
            .unwrap();

        service_stack.ready().await.unwrap();
        assert!(
            service_stack
                .call(ExecutionRequest::fake_builder().build())
                .await
                .is_ok()
        );
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    /// The point of this layer: the inner service is never cloned.
    #[tokio::test]
    async fn test_continue_does_not_clone_inner_service() {
        let clones = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = crate::plugin::test::CountClones::new(
            tower::service_fn(|req: ExecutionRequest| async move {
                Ok::<_, BoxError>(
                    ExecutionResponse::fake_builder()
                        .context(req.context)
                        .build()
                        .unwrap(),
                )
            }),
            clones.clone(),
        );

        let mut service_stack =
            CheckpointLayer::new(|req| Ok(ControlFlow::Continue(req))).layer(inner);
        for _ in 0..2 {
            service_stack
                .ready()
                .await
                .unwrap()
                .call(ExecutionRequest::fake_builder().build())
                .await
                .unwrap();
        }

        assert_eq!(clones.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
