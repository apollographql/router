use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use tower::Service;

use super::future::ResponseFuture;
use super::Rate;
use crate::plugins::traffic_shaping::rate::error::RateLimited;

#[derive(Debug, Clone)]
pub(crate) struct RateLimit<T> {
    pub(crate) inner: T,
    pub(crate) rate: Rate,
    /// We're using an atomic u64 because it's basically a timestamp in milliseconds for the start of the window
    /// Instead of using an Instant which is not thread safe we're using an atomic u64
    /// It's ok to have an u64 because we just care about milliseconds for this use case
    pub(crate) window_start: Arc<AtomicU64>,
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
        let time_unit = self.rate.per().as_millis() as u64;

        let updated =
            self.window_start
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |window_start| {
                    let duration_now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("system time must be after EPOCH")
                        .as_millis() as u64;
                    if duration_now - window_start > self.rate.per().as_millis() as u64 {
                        Some(duration_now)
                    } else {
                        None
                    }
                });
        // If it has been updated
        if let Ok(_updated_window_start) = updated {
            self.previous_nb_requests.swap(
                self.current_nb_requests.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            self.current_nb_requests.swap(1, Ordering::SeqCst);
        }

        let estimated_cap = (self.previous_nb_requests.load(Ordering::SeqCst)
            * (time_unit
                .checked_sub(self.window_start.load(Ordering::SeqCst))
                .unwrap_or_default()
                / time_unit) as usize)
            + self.current_nb_requests.load(Ordering::SeqCst);

        if estimated_cap as u64 > self.rate.num() {
            tracing::trace!("rate limit exceeded; sleeping.");
            return Poll::Ready(Err(RateLimited::new().into()));
        }

        self.current_nb_requests.fetch_add(1, Ordering::SeqCst);

        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        ResponseFuture::new(self.inner.call(request))
    }
}
