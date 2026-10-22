//! Logging layers that keep locks off span callbacks
//!
//! Span callbacks (`on_enter`, `on_exit` and the rest) run on every poll of every instrumented
//! future, on every worker thread. Two logging layers of the subscriber that
//! [`init_telemetry`](super::otel::init_telemetry) installs take a lock in them: the
//! `reload::Layer` around the log format layer, and `EnvFilter`, for its span-scoped directives.
//! With every worker thread contending on those locks, they would be the largest part of what
//! spans cost. [`EventsOnly`] and [`LogFilter`] keep span callbacks away from them when they have
//! nothing to do there.

use tracing_core::Dispatch;
use tracing_core::Event;
use tracing_core::Interest;
use tracing_core::LevelFilter;
use tracing_core::Metadata;
use tracing_core::Subscriber;
use tracing_core::span;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::ParseError;
use tracing_subscriber::layer::Context;

/// Passes only events, and the filtering around them, to the layer it wraps.
///
/// The log format layer sits behind a `reload::Layer` so that a configuration change can swap it,
/// and `reload::Layer` takes a read lock on every callback. The format layer only needs events:
/// the span fields it shows are recorded by
/// [`SpanFieldsLayer`](crate::plugins::telemetry::fmt_layer::SpanFieldsLayer), outside the lock. A
/// layer reloaded behind this one gets no span callbacks.
///
/// Downcasts don't reach the wrapped layer. `reload::Layer` doesn't allow them through either.
pub(crate) struct EventsOnly<L>(L);

impl<L> EventsOnly<L> {
    pub(crate) fn new(layer: L) -> Self {
        Self(layer)
    }
}

impl<S: Subscriber, L: Layer<S>> Layer<S> for EventsOnly<L> {
    fn on_register_dispatch(&self, subscriber: &Dispatch) {
        self.0.on_register_dispatch(subscriber)
    }

    fn on_layer(&mut self, subscriber: &mut S) {
        self.0.on_layer(subscriber)
    }

    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        self.0.register_callsite(metadata)
    }

    fn enabled(&self, metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        self.0.enabled(metadata, ctx)
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.0.max_level_hint()
    }

    fn event_enabled(&self, event: &Event<'_>, ctx: Context<'_, S>) -> bool {
        self.0.event_enabled(event, ctx)
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        self.0.on_event(event, ctx)
    }
}

/// An [`EnvFilter`] that tracks spans only when one of its directives needs them.
///
/// A directive like `apollo_router[subgraph]=debug` matches events inside a span, so `EnvFilter`
/// keeps a table of spans and takes a lock on it on every span enter and exit. It does that even
/// when no directive names a span or a field, and the table stays empty. Directives can only name
/// one inside `[...]`, so without a `[` this filter skips the span callbacks.
pub(crate) struct LogFilter {
    filter: EnvFilter,
    tracks_spans: bool,
}

impl LogFilter {
    pub(crate) fn try_new(directives: &str) -> Result<Self, ParseError> {
        Ok(Self {
            filter: EnvFilter::try_new(directives)?,
            tracks_spans: directives.contains('['),
        })
    }
}

impl<S: Subscriber> Layer<S> for LogFilter {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        Layer::<S>::register_callsite(&self.filter, metadata)
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Layer::<S>::max_level_hint(&self.filter)
    }

    fn enabled(&self, metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        Layer::<S>::enabled(&self.filter, metadata, ctx)
    }

    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        if self.tracks_spans {
            Layer::<S>::on_new_span(&self.filter, attrs, id, ctx)
        }
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        if self.tracks_spans {
            Layer::<S>::on_record(&self.filter, id, values, ctx)
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if self.tracks_spans {
            Layer::<S>::on_enter(&self.filter, id, ctx)
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if self.tracks_spans {
            Layer::<S>::on_exit(&self.filter, id, ctx)
        }
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        if self.tracks_spans {
            Layer::<S>::on_close(&self.filter, id, ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// Counts the events and span callbacks it receives.
    #[derive(Clone, Default)]
    struct Counter {
        events: Arc<AtomicUsize>,
        span_callbacks: Arc<AtomicUsize>,
    }

    impl Counter {
        fn events(&self) -> usize {
            self.events.load(Ordering::Relaxed)
        }

        fn span_callbacks(&self) -> usize {
            self.span_callbacks.load(Ordering::Relaxed)
        }
    }

    impl<S: Subscriber> Layer<S> for Counter {
        fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {
            self.events.fetch_add(1, Ordering::Relaxed);
        }

        fn on_new_span(&self, _attrs: &span::Attributes<'_>, _id: &span::Id, _ctx: Context<'_, S>) {
            self.span_callbacks.fetch_add(1, Ordering::Relaxed);
        }

        fn on_enter(&self, _id: &span::Id, _ctx: Context<'_, S>) {
            self.span_callbacks.fetch_add(1, Ordering::Relaxed);
        }

        fn on_exit(&self, _id: &span::Id, _ctx: Context<'_, S>) {
            self.span_callbacks.fetch_add(1, Ordering::Relaxed);
        }

        fn on_close(&self, _id: span::Id, _ctx: Context<'_, S>) {
            self.span_callbacks.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn events_only_passes_events_but_not_span_callbacks() {
        let counter = Counter::default();
        let subscriber = Registry::default().with(EventsOnly::new(counter.clone()));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            tracing::info!("inside the span");
        });

        assert_eq!(counter.events(), 1);
        assert_eq!(counter.span_callbacks(), 0);
    }

    #[test]
    fn log_filter_tracks_spans_only_for_directives_that_name_one() {
        let tracks = |directives| LogFilter::try_new(directives).unwrap().tracks_spans;
        assert!(!tracks("info,apollo_router=debug,opentelemetry=warn"));
        assert!(tracks("info,apollo_router[subgraph]=debug"));
        assert!(tracks("info,[{subgraph.name=accounts}]=debug"));
    }

    #[test]
    fn log_filter_filters_by_level_without_tracking_spans() {
        let counter = Counter::default();
        let subscriber = Registry::default()
            .with(counter.clone())
            .with(LogFilter::try_new("info").unwrap());

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            tracing::debug!("filtered out");
            tracing::info!("kept");
        });

        assert_eq!(counter.events(), 1);
    }

    #[test]
    fn log_filter_matches_span_scoped_directives() {
        let counter = Counter::default();
        let subscriber = Registry::default()
            .with(counter.clone())
            .with(LogFilter::try_new("info,[subgraph]=debug").unwrap());

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!("outside the span: filtered out");
            let span = tracing::info_span!("subgraph");
            let _entered = span.enter();
            tracing::debug!("inside the span: kept");
        });

        assert_eq!(counter.events(), 1);
    }
}
