use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use futures::ready;
use tower::Service;

use super::future::ResponseFuture;
use super::Rate;
use crate::plugins::traffic_shaping::rate::error::RateLimited;

// FIXME: better name
#[derive(Debug, Clone)]
pub(crate) enum RateLimiting<T> {
    Apollo(RateLimit<T>),
    User(RateLimit<T>),
}

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

impl<S, Request> Service<Request> for RateLimiting<S>
where
    S: Service<Request>,
    S::Error: Into<tower::BoxError>,
{
    type Response = S::Response;
    type Error = tower::BoxError;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let rate_limiting = match self {
            RateLimiting::Apollo(blah) => blah,
            RateLimiting::User(blah) => blah,
        };

        let time_unit = rate_limiting.rate.per().as_millis() as u64;

        let updated = rate_limiting.window_start.fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |window_start| {
                let duration_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time must be after EPOCH")
                    .as_millis() as u64;
                if duration_now - window_start > rate_limiting.rate.per().as_millis() as u64 {
                    Some(duration_now)
                } else {
                    None
                }
            },
        );
        // If it has been updated
        if let Ok(_updated_window_start) = updated {
            rate_limiting.previous_nb_requests.swap(
                rate_limiting.current_nb_requests.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            rate_limiting.current_nb_requests.swap(1, Ordering::SeqCst);
        }

        let estimated_cap = (rate_limiting.previous_nb_requests.load(Ordering::SeqCst)
            * (time_unit
                .checked_sub(rate_limiting.window_start.load(Ordering::SeqCst))
                .unwrap_or_default()
                / time_unit) as usize)
            + rate_limiting.current_nb_requests.load(Ordering::SeqCst);

        if estimated_cap as u64 > rate_limiting.rate.num() {
            tracing::trace!("rate limit exceeded; sleeping.");
            return Poll::Ready(Err(RateLimited::new().into()));
        }

        rate_limiting
            .current_nb_requests
            .fetch_add(1, Ordering::SeqCst);

        Poll::Ready(ready!(rate_limiting.inner.poll_ready(cx)).map_err(Into::into))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let rate_limiting = match self {
            RateLimiting::Apollo(blah) => blah,
            RateLimiting::User(blah) => blah,
        };
        ResponseFuture::new(rate_limiting.inner.call(request))
    }
}
