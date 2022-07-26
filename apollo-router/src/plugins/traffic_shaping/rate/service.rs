use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::RwLock;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use futures::future::FutureExt;
use futures::ready;
use tokio::time::sleep;
use tokio::time::sleep_until;
use tokio::time::Instant;
use tokio::time::Sleep;
use tower::Service;

use super::future::ResponseFuture;
use super::Rate;
use crate::plugins::traffic_shaping::rate::error::RateLimited;

#[derive(Debug)]
pub(crate) struct RateLimit<T> {
    pub(crate) inner: T,
    pub(crate) rate: Rate,
    pub(crate) curr_time: Arc<RwLock<Instant>>,
    pub(crate) previous_counter: Arc<AtomicUsize>,
    pub(crate) current_counter: Arc<AtomicUsize>,
}

impl<S, Request> Service<Request> for RateLimit<S>
where
    S: Service<Request>,
    S::Error: Into<tower::BoxError>,
{
    type Response = S::Response;
    type Error = tower::BoxError;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let curr_time_instant = *self.curr_time.read().unwrap();
        let curr_time = curr_time_instant.elapsed();
        let time_unit = self.rate.per();

        if curr_time > time_unit {
            *self.curr_time.write().unwrap() = Instant::now();
            self.previous_counter.swap(
                self.current_counter.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            self.current_counter.swap(1, Ordering::SeqCst);
        }
        let estimated_cap = (self.previous_counter.load(Ordering::SeqCst)
            * (time_unit
                .checked_sub(curr_time)
                .unwrap_or_default()
                .as_millis()
                / time_unit.as_millis()) as usize)
            + self.current_counter.load(Ordering::SeqCst);

        if estimated_cap as u64 > self.rate.num() {
            tracing::trace!("rate limit exceeded; sleeping.");
            return Poll::Ready(Err(RateLimited::new().into()));
        }

        self.current_counter.fetch_add(1, Ordering::SeqCst);

        Poll::Ready(ready!(self.inner.poll_ready(cx)).map_err(Into::into))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        ResponseFuture::new(self.inner.call(request))
    }
}
