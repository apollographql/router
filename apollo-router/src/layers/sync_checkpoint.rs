//! A checkpoint whose decision awaits nothing, made in `call`, which calls the service beneath it
//! in place, or, where it answers the request itself, drops that readied service for a fresh clone
//! ([`release_readied`]).
//!
//! An [`AsyncCheckpointService`](super::async_checkpoint::AsyncCheckpointService) awaits its
//! decision in the future it returns, so it clones the service beneath it on every call to move
//! the readied one into that future, and boxes the future; the clone copies every layer down to
//! the next boundary. A decision that awaits nothing is made where the request is called: the
//! readied service beneath is called with the request it continues, or the response it breaks
//! with is answered, and nothing is cloned or boxed.
//!
//! Upstream's open `#10669` (`layers/sync_checkpoint.rs`, `SyncCheckpointLayer`) is the prior art,
//! taken by reference.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::future::Either;
use futures::future::Ready;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use super::release_readied;

/// A checkpoint's decision: `Continue` with the request the service beneath is called with,
/// `Break` with the response answered in its place, or an error.
pub(crate) type Checkpoint<Request, Response> =
    Arc<dyn Fn(Request) -> Result<ControlFlow<Response, Request>, BoxError> + Send + Sync>;

/// The [`Layer`] of a [`SyncCheckpointService`].
pub(crate) struct SyncCheckpointLayer<Request, Response> {
    checkpoint: Checkpoint<Request, Response>,
}

impl<Request, Response> SyncCheckpointLayer<Request, Response> {
    pub(crate) fn new(
        checkpoint: impl Fn(Request) -> Result<ControlFlow<Response, Request>, BoxError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            checkpoint: Arc::new(checkpoint),
        }
    }
}

impl<S, Request> Layer<S> for SyncCheckpointLayer<Request, S::Response>
where
    S: Service<Request, Error = BoxError>,
{
    type Service = SyncCheckpointService<S, Request>;

    fn layer(&self, inner: S) -> Self::Service {
        SyncCheckpointService {
            inner,
            checkpoint: Arc::clone(&self.checkpoint),
        }
    }
}

/// Decides each request where it is called. See the [module documentation](self).
pub(crate) struct SyncCheckpointService<S, Request>
where
    S: Service<Request>,
{
    inner: S,
    checkpoint: Checkpoint<Request, S::Response>,
}

impl<S, Request> Clone for SyncCheckpointService<S, Request>
where
    S: Service<Request> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            checkpoint: Arc::clone(&self.checkpoint),
        }
    }
}

impl<S, Request> Service<Request> for SyncCheckpointService<S, Request>
where
    S: Service<Request, Error = BoxError> + Clone,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Either<Ready<Result<S::Response, BoxError>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let answered = match (self.checkpoint)(request) {
            Ok(ControlFlow::Continue(request)) => return Either::Right(self.inner.call(request)),
            Ok(ControlFlow::Break(response)) => Ok(response),
            Err(error) => Err(error),
        };
        // answered without the service beneath, which its caller readied
        release_readied(&mut self.inner);
        Either::Left(futures::future::ready(answered))
    }
}
