//! A checkpoint whose decision is synchronous, so it calls the service beneath it directly.
//!
//! An [`AsyncCheckpointService`](super::async_checkpoint::AsyncCheckpointService) must clone the
//! service beneath it on every call, to move it into the future that awaits the decision. A
//! synchronous decision is made inside `call`, so the service beneath is called in place and
//! never cloned.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::future::Either;
use futures::future::Ready;
use tower::BoxError;
use tower::Layer;
use tower::Service;

type CheckpointFn<Request, Response> =
    Arc<dyn Fn(Request) -> Result<ControlFlow<Response, Request>, BoxError> + Send + Sync>;

/// [`Layer`] for [`SyncCheckpointService`].
pub(crate) struct SyncCheckpointLayer<Request, Response> {
    checkpoint_fn: CheckpointFn<Request, Response>,
}

impl<Request, Response> SyncCheckpointLayer<Request, Response> {
    /// Creates a layer that runs `checkpoint_fn` on each request: `Continue` passes the request
    /// on to the service beneath, and `Break` or an error answers it without calling that service.
    pub(crate) fn new(
        checkpoint_fn: impl Fn(Request) -> Result<ControlFlow<Response, Request>, BoxError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            checkpoint_fn: Arc::new(checkpoint_fn),
        }
    }
}

impl<S, Request, Response> Layer<S> for SyncCheckpointLayer<Request, Response>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    type Service = SyncCheckpointService<S, Request>;

    fn layer(&self, inner: S) -> Self::Service {
        SyncCheckpointService {
            inner,
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
        }
    }
}

/// Runs a synchronous checkpoint before the service beneath. See [`SyncCheckpointLayer::new`].
pub(crate) struct SyncCheckpointService<S, Request>
where
    S: Service<Request>,
{
    inner: S,
    checkpoint_fn: CheckpointFn<Request, S::Response>,
}

impl<S, Request> Clone for SyncCheckpointService<S, Request>
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

impl<S, Request> Service<Request> for SyncCheckpointService<S, Request>
where
    S: Service<Request, Error = BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Either<Ready<Result<S::Response, BoxError>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        match (self.checkpoint_fn)(request) {
            Ok(ControlFlow::Continue(request)) => Either::Right(self.inner.call(request)),
            Ok(ControlFlow::Break(response)) => Either::Left(futures::future::ok(response)),
            Err(error) => Either::Left(futures::future::err(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use super::*;

    /// Answers each request with the request itself and counts how many times it is cloned.
    #[derive(Default)]
    struct Echo {
        clones: Arc<AtomicUsize>,
    }

    impl Clone for Echo {
        fn clone(&self) -> Self {
            self.clones.fetch_add(1, Ordering::Relaxed);
            Self {
                clones: Arc::clone(&self.clones),
            }
        }
    }

    impl Service<u32> for Echo {
        type Response = u32;
        type Error = BoxError;
        type Future = Ready<Result<u32, BoxError>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: u32) -> Self::Future {
            futures::future::ok(request)
        }
    }

    /// Continues even requests, answers odd ones with 0, and fails on 3.
    fn checkpointed(echo: Echo) -> SyncCheckpointService<Echo, u32> {
        ServiceBuilder::new()
            .layer(SyncCheckpointLayer::new(|request: u32| match request {
                3 => Err("three".into()),
                request if request % 2 == 1 => Ok(ControlFlow::Break(0)),
                request => Ok(ControlFlow::Continue(request)),
            }))
            .service(echo)
    }

    #[tokio::test]
    async fn continue_reaches_the_service_beneath() {
        let response = checkpointed(Echo::default()).oneshot(2).await.unwrap();
        assert_eq!(response, 2);
    }

    #[tokio::test]
    async fn break_answers_without_the_service_beneath() {
        let response = checkpointed(Echo::default()).oneshot(1).await.unwrap();
        assert_eq!(response, 0);
    }

    #[tokio::test]
    async fn an_error_answers_without_the_service_beneath() {
        let error = checkpointed(Echo::default()).oneshot(3).await.unwrap_err();
        assert_eq!(error.to_string(), "three");
    }

    #[tokio::test]
    async fn calls_do_not_clone_the_service_beneath() {
        let echo = Echo::default();
        let clones = Arc::clone(&echo.clones);
        let mut service = checkpointed(echo);

        for request in [1, 2, 4] {
            service.ready().await.unwrap().call(request).await.unwrap();
        }

        assert_eq!(clones.load(Ordering::Relaxed), 0);
    }
}
