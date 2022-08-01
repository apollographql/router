use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::RwLock;
use std::task::Context;
use std::task::Poll;

use futures::ready;
use tokio::time::Instant;
use tower::Service;

use super::future::ResponseFuture;
use super::Rate;
use crate::plugins::traffic_shaping::rate::error::RateLimited;

#[derive(Debug)]
pub(crate) struct RateLimit<T> {
    pub(crate) inner: T,
    pub(crate) rate: Rate,
    pub(crate) window_start: Arc<RwLock<Instant>>,
    pub(crate) previous_nb_requests: Arc<AtomicUsize>,
    pub(crate) current_nb_requests: Arc<AtomicUsize>,
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
        let mut window_start = self.window_start.read().unwrap().elapsed();
        let time_unit = self.rate.per();

        if window_start > time_unit {
            let new_window_start = Instant::now();
            *self.window_start.write().unwrap() = new_window_start;
            window_start = new_window_start.elapsed();
            self.previous_nb_requests.swap(
                self.current_nb_requests.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            self.current_nb_requests.swap(1, Ordering::SeqCst);
        }
        let estimated_cap = (self.previous_nb_requests.load(Ordering::SeqCst)
            * (time_unit
                .checked_sub(window_start)
                .unwrap_or_default()
                .as_millis()
                / time_unit.as_millis()) as usize)
            + self.current_nb_requests.load(Ordering::SeqCst);

        if estimated_cap as u64 > self.rate.num() {
            tracing::trace!("rate limit exceeded; sleeping.");
            return Poll::Ready(Err(RateLimited::new().into()));
        }

        self.current_nb_requests.fetch_add(1, Ordering::SeqCst);

        Poll::Ready(ready!(self.inner.poll_ready(cx)).map_err(Into::into))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        ResponseFuture::new(self.inner.call(request))
    }
}
