use std::sync::Arc;
use std::sync::Mutex;
use std::task::Poll;
use std::time::Duration;

use futures::prelude::*;
use tokio_util::time::DelayQueue;

use crate::router::Event;

#[derive(Default)]
struct ReloadSourceInner {
    queue: DelayQueue<()>,
    period: Option<Duration>,
}

/// Reload source is an internal event emitter for the state machine that will send reload events on SIGUP and/or on a timer.
#[derive(Clone, Default)]
pub(crate) struct ReloadSource {
    inner: Arc<Mutex<ReloadSourceInner>>,
}

impl ReloadSource {
    pub(crate) fn set_period(&self, period: &Option<Duration>) {
        let mut inner = self.inner.lock().unwrap();
        // Clear the queue before setting the period
        inner.queue.clear();
        inner.period = *period;
        if let Some(period) = period {
            inner.queue.insert((), *period);
        }
    }

    pub(crate) fn into_stream(self) -> impl Stream<Item = Event> {
        #[cfg(unix)]
        let signal_stream = {
            let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("Failed to install SIGHUP signal handler");

            futures::stream::poll_fn(move |cx| match signal.poll_recv(cx) {
                Poll::Ready(Some(_)) => Poll::Ready(Some(Event::Reload)),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            })
            .boxed()
        };
        #[cfg(not(unix))]
        let signal_stream = futures::stream::empty().boxed();

        let periodic_reload = futures::stream::poll_fn(move |cx| {
            let mut inner = self.inner.lock().unwrap();
            match inner.queue.poll_expired(cx) {
                Poll::Ready(Some(_expired)) => {
                    if let Some(period) = inner.period {
                        inner.queue.insert((), period);
                    }
                    Poll::Ready(Some(Event::Reload))
                }
                // We must return pending even if the queue is empty, otherwise the stream will never be polled again
                // The waker will still be used, so this won't end up in a hot loop.
                Poll::Ready(None) => Poll::Pending,
                Poll::Pending => Poll::Pending,
            }
        });

        futures::stream::select(signal_stream, periodic_reload)
    }
}
