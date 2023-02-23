//! Our formatters and visitors used for logging
pub(crate) mod json;
pub(crate) mod text;

use std::fmt;

use tracing::Subscriber;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::registry::LookupSpan;

use super::metrics::METRIC_PREFIX_COUNTER;
use super::metrics::METRIC_PREFIX_HISTOGRAM;
use super::metrics::METRIC_PREFIX_MONOTONIC_COUNTER;
use super::metrics::METRIC_PREFIX_VALUE;

pub(crate) const TRACE_ID_FIELD_NAME: &str = "trace_id";

/// `FilteringFormatter` is useful if you want to not filter the entire event but only want to not display it
/// ```ignore
/// use tracing_core::Event;
/// use tracing_subscriber::fmt::format::{Format};
/// tracing_subscriber::fmt::fmt()
/// .event_format(FilteringFormatter::new(
///     Format::default().pretty(),
///     // Do not display the event if an attribute name starts with "counter"
///     |event: &Event| !event.metadata().fields().iter().any(|f| f.name().starts_with("counter")),
/// ))
/// .finish();
/// ```
pub(crate) struct FilteringFormatter<T, F> {
    inner: T,
    filter_fn: F,
}

impl<T, F> FilteringFormatter<T, F>
where
    F: Fn(&tracing::Event<'_>) -> bool,
{
    pub(crate) fn new(inner: T, filter_fn: F) -> Self {
        Self { inner, filter_fn }
    }
}

impl<T, F, S, N> FormatEvent<S, N> for FilteringFormatter<T, F>
where
    T: FormatEvent<S, N>,
    F: Fn(&tracing::Event<'_>) -> bool,
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        if (self.filter_fn)(event) {
            self.inner.format_event(ctx, writer, event)
        } else {
            Ok(())
        }
    }
}

// Function to filter metric event for the filter formatter
pub(crate) fn filter_metric_events(event: &tracing::Event<'_>) -> bool {
    !event.metadata().fields().iter().any(|f| {
        f.name().starts_with(METRIC_PREFIX_COUNTER)
            || f.name().starts_with(METRIC_PREFIX_HISTOGRAM)
            || f.name().starts_with(METRIC_PREFIX_MONOTONIC_COUNTER)
            || f.name().starts_with(METRIC_PREFIX_VALUE)
    })
}
