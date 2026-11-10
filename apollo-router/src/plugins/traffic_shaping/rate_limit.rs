//! Traffic shaping's rate limit, charged where a request is called, without a buffer.
//!
//! Tower's `RateLimit` keeps its window in the service, so a stack cloned per request would give
//! each clone a counter of its own; stock shared one by putting the stack behind a buffer, whose
//! single worker readied the rate limit and shed through load shedding. Here the rate limit of a
//! built service is held behind a mutex every clone of that service shares, and a request is
//! charged in `call`: its readiness is polled once with a waker that does nothing, and a request
//! it is not ready for is refused at once with [`Overloaded`], which the layers above answer as
//! load shedding's refusal was answered. Tower's own window and timer decide, one counter per
//! built service as before.

use std::convert::Infallible;
use std::future::Future;
use std::future::Ready;
use std::future::ready;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::future::Either;
use parking_lot::Mutex;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::limit::RateLimit;
use tower::limit::RateLimitLayer;
use tower::load_shed::error::Overloaded;

use crate::layers::release_readied;

/// The service a rate limit is charged through: it does nothing.
#[derive(Clone, Copy)]
pub(crate) struct Charge;

impl Service<()> for Charge {
    type Response = ();
    type Error = Infallible;
    type Future = Ready<Result<(), Infallible>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: ()) -> Self::Future {
        ready(Ok(()))
    }
}

/// A built service's rate limit, shared by every clone of that service.
pub(crate) type Limiter = Arc<Mutex<RateLimit<Charge>>>;

/// A limiter with `layer`'s rate: a counter of its own, as each service built from tower's layer
/// has one.
pub(crate) fn limiter(layer: &RateLimitLayer) -> Limiter {
    Arc::new(Mutex::new(layer.layer(Charge)))
}

/// Charges `limiter` for one request, answering whether the request is admitted.
///
/// The readiness is polled outside Tokio's cooperative budget, as `UnconstrainedBuffer` polls the
/// buffer stock put behind its load shedding: tower's rate limit waits out its window on a timer,
/// and a timer polled once the request's task has spent its budget answers `Pending` though the
/// window has passed, which would refuse a request the window admits.
pub(crate) fn admits(limiter: &Limiter) -> bool {
    let mut limiter = limiter.lock();
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    let polled = std::pin::pin!(tokio::task::unconstrained(std::future::poll_fn(|cx| {
        limiter.poll_ready(cx)
    })))
    .poll(&mut cx);
    match polled {
        Poll::Ready(Ok(())) => {
            // the window is charged when the rate limit is called; its future does nothing
            drop(limiter.call(()));
            true
        }
        Poll::Ready(Err(never)) => match never {},
        Poll::Pending => false,
    }
}

/// The [`Layer`] of a [`SharedRateLimit`]: each service it builds has a counter of its own.
#[derive(Clone)]
pub(crate) struct SharedRateLimitLayer {
    rate: RateLimitLayer,
}

impl SharedRateLimitLayer {
    pub(super) fn new(rate: RateLimitLayer) -> Self {
        Self { rate }
    }
}

impl<S> Layer<S> for SharedRateLimitLayer {
    type Service = SharedRateLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SharedRateLimit {
            inner,
            limiter: limiter(&self.rate),
        }
    }
}

/// A service whose requests are charged against one rate limit its clones share. See the
/// [module documentation](self).
#[derive(Clone)]
pub(crate) struct SharedRateLimit<S> {
    inner: S,
    limiter: Limiter,
}

impl<S, Request> Service<Request> for SharedRateLimit<S>
where
    S: Service<Request, Error = BoxError> + Clone,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Either<S::Future, Ready<Result<S::Response, BoxError>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        if admits(&self.limiter) {
            return Either::Left(self.inner.call(request));
        }
        // refused without the service beneath, which may hold what its readiness reserved
        release_readied(&mut self.inner);
        Either::Right(ready(Err(Overloaded::new().into())))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::task::coop::poll_proceed;
    use tower::ServiceExt as _;

    use super::*;

    /// A request the window admits is admitted with its task's cooperative budget spent: a rate
    /// limit of one request per 50 ms refuses a second request at once, and once the window has
    /// passed, a request charged with the budget spent is admitted. Tower's rate limit waits out
    /// its window on a timer, which answers `Pending` without budget though the window has
    /// passed; read within the budget, that `Pending` refused the request.
    #[tokio::test(start_paused = true)]
    async fn a_request_the_window_admits_is_admitted_with_its_tasks_budget_spent() {
        let limiter = limiter(&RateLimitLayer::new(1, Duration::from_millis(50)));
        assert!(admits(&limiter), "the window's first request");
        assert!(!admits(&limiter), "a second request in the same window");

        tokio::time::advance(Duration::from_millis(60)).await;
        tokio::task::yield_now().await;
        let admitted = std::future::poll_fn(|cx| {
            while let Poll::Ready(restore) = poll_proceed(cx) {
                restore.made_progress();
            }
            Poll::Ready(admits(&limiter))
        })
        .await;

        assert!(
            admitted,
            "a request in the next window, charged with its task's budget spent"
        );
    }

    /// Clones of a built service share its counter, and a refused request answers `Overloaded`
    /// without calling the service beneath.
    #[tokio::test]
    async fn clones_share_the_counter_and_a_refusal_is_overloaded() {
        let service = SharedRateLimitLayer::new(RateLimitLayer::new(1, Duration::from_secs(60)))
            .layer(tower::service_fn(|request: u32| async move {
                Ok::<_, BoxError>(request)
            }));
        let first = service.clone().oneshot(1).await.expect("the first request");
        let second = service.clone().oneshot(2).await;

        assert_eq!(first, 1);
        assert!(
            second.is_err_and(|error| error.is::<Overloaded>()),
            "a second request through another clone of the service, in the same window"
        );
    }
}
