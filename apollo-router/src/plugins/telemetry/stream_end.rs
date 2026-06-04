//! Stream combinator that fires a one-shot callback when a `Stream` completes normally
//! (i.e. `poll_next` returns `Poll::Ready(None)`). Reusable plumbing for driving
//! `on_stream_end` telemetry hooks — see
//! `crate::plugins::telemetry::config_new::instruments::Instrumented::on_stream_end`.
//!
//! Drop without normal completion deliberately does not fire the callback: a cancelled
//! or client-disconnected stream has no well-defined lifecycle duration. For metrics that
//! *must* also record on drop (e.g. `http.server.request.duration`), see
//! `crate::plugins::telemetry::config_new::router::instruments::RequestDurationBody`, which
//! pairs an end-of-body callback with a `Drop`-based guard.
//!
//! This combinator is currently only exercised by its own unit tests; it is retained as
//! reusable infrastructure for future stream-anchored instruments.
#![allow(dead_code)]

use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use futures::Stream;
use pin_project_lite::pin_project;

pin_project! {
    pub(crate) struct StreamEndObserver<S, F>
    where
        F: FnOnce(),
    {
        #[pin]
        inner: S,
        on_end: Option<F>,
    }
}

impl<S, F> StreamEndObserver<S, F>
where
    F: FnOnce(),
{
    pub(crate) fn new(inner: S, on_end: F) -> Self {
        Self {
            inner,
            on_end: Some(on_end),
        }
    }
}

impl<S, F> Stream for StreamEndObserver<S, F>
where
    S: Stream,
    F: FnOnce(),
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match this.inner.poll_next(cx) {
            Poll::Ready(None) => {
                if let Some(cb) = this.on_end.take() {
                    cb();
                }
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use futures::StreamExt;
    use futures::stream;

    use super::StreamEndObserver;

    #[tokio::test]
    async fn fires_once_on_normal_completion() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_cb = count.clone();
        let inner = stream::iter(vec![1, 2, 3]);
        let observed = StreamEndObserver::new(inner, move || {
            count_for_cb.fetch_add(1, Ordering::SeqCst);
        });

        let items: Vec<i32> = observed.collect().await;

        assert_eq!(items, vec![1, 2, 3]);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_fire_when_dropped_before_completion() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_cb = count.clone();
        let inner = stream::iter(vec![1, 2, 3]);
        let mut observed = Box::pin(StreamEndObserver::new(inner, move || {
            count_for_cb.fetch_add(1, Ordering::SeqCst);
        }));

        // Pull a single item then drop the stream — emulates a client disconnect
        // partway through the response.
        assert_eq!(observed.next().await, Some(1));
        drop(observed);

        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
