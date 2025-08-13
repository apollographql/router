#[cfg(unix)]
use std::task::Poll;

use futures::prelude::*;

use crate::router::Event;

/// Reload source is an internal event emitter for the state machine that will send reload events on SIGHUP
#[derive(Clone, Default)]
pub(crate) struct ReloadSource;

impl ReloadSource {
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

        signal_stream
    }
}

/// Extension trait to add chaos reload functionality to event streams.
///
/// This trait provides the `.with_sighub_reload()` method that automatically triggers a reload event when SIGHUP is received.
pub(crate) trait ReloadableEventStream: Stream<Item = Event> + Sized {
    /// Adds sighub reload to the event stream.
    fn with_sighup_reload(self) -> impl Stream<Item = Event> {
        stream::select(self, ReloadSource.into_stream())
    }
}

impl<S> ReloadableEventStream for S where S: Stream<Item = Event> {}
