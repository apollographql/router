use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use std::task::Context;
use std::task::Poll;

use futures::ready;
use tokio::time::Instant;
use tokio::time::Sleep;
use tower::Service;

use super::Rate;

/// Enforces a rate limit on the number of requests the underlying
/// service can handle over a period of time.
#[derive(Debug)]
pub(crate) struct RateLimit<T> {
    pub(crate) inner: T,
    pub(crate) rate: Rate,
    pub(crate) state: Arc<RwLock<State>>,
    pub(crate) sleep: Arc<RwLock<Pin<Box<Sleep>>>>,
}

#[derive(Debug)]
pub(crate) enum State {
    // The service has hit its limit
    Limited,
    Ready { until: Instant, rem: u64 },
}

impl<S, Request> Service<Request> for RateLimit<S>
where
    S: Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match &*self.state.read().unwrap() {
            State::Ready { .. } => return Poll::Ready(ready!(self.inner.poll_ready(cx))),
            State::Limited => {
                if Pin::new(&mut self.sleep.write().unwrap().as_mut())
                    .poll(cx)
                    .is_pending()
                {
                    tracing::trace!("rate limit exceeded; sleeping.");
                    return Poll::Pending;
                }
            }
        }

        *self.state.write().unwrap() = State::Ready {
            until: Instant::now() + self.rate.per(),
            rem: self.rate.num(),
        };

        Poll::Ready(ready!(self.inner.poll_ready(cx)))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let mut state = self.state.write().unwrap();
        match &*state {
            State::Ready { mut until, mut rem } => {
                let now = Instant::now();

                // If the period has elapsed, reset it.
                if now >= until {
                    until = now + self.rate.per();
                    rem = self.rate.num();
                }

                if rem > 1 {
                    rem -= 1;
                    *state = State::Ready { until, rem };
                } else {
                    // The service is disabled until further notice
                    // Reset the sleep future in place, so that we don't have to
                    // deallocate the existing box and allocate a new one.
                    self.sleep.write().unwrap().as_mut().reset(until);
                    *state = State::Limited;
                }
                drop(state);

                // Call the inner future
                self.inner.call(request)
            }
            State::Limited => panic!("service not ready; poll_ready must be called first"),
        }
    }
}

#[cfg(feature = "load")]
#[cfg_attr(docsrs, doc(cfg(feature = "load")))]
impl<S> crate::load::Load for RateLimit<S>
where
    S: crate::load::Load,
{
    type Metric = S::Metric;
    fn load(&self) -> Self::Metric {
        self.inner.load()
    }
}
