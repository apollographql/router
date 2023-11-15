//! Our formatters and visitors used for logging
pub(crate) mod json;
pub(crate) mod text;

use std::collections::HashMap;
use std::fmt;

use opentelemetry::sdk::Resource;
use serde_json::Number;
use tracing::Subscriber;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::metrics::layer::METRIC_PREFIX_COUNTER;
use crate::metrics::layer::METRIC_PREFIX_HISTOGRAM;
use crate::metrics::layer::METRIC_PREFIX_MONOTONIC_COUNTER;
use crate::metrics::layer::METRIC_PREFIX_VALUE;

pub(crate) const APOLLO_PRIVATE_PREFIX: &str = "apollo_private.";
// This list comes from Otel https://opentelemetry.io/docs/specs/semconv/attributes-registry/code/ and
pub(crate) const EXCLUDED_ATTRIBUTES: [&str; 5] = [
    "code.filepath",
    "code.namespace",
    "code.lineno",
    "thread.id",
    "thread.name",
];

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

impl<T, F, S> EventFormatter<S> for FilteringFormatter<T, F>
where
    T: EventFormatter<S>,
    F: Fn(&tracing::Event<'_>) -> bool,
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn format_event<W>(
        &self,
        ctx: &Context<'_, S>,
        writer: &mut W,
        event: &tracing::Event<'_>,
    ) -> fmt::Result
    where
        W: std::fmt::Write,
    {
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

pub(crate) fn to_map(resource: Resource) -> HashMap<String, serde_json::Value> {
    resource
        .into_iter()
        .map(|(k, v)| {
            (
                k.into(),
                match v {
                    opentelemetry::Value::Bool(value) => serde_json::Value::Bool(value),
                    opentelemetry::Value::I64(value) => {
                        serde_json::Value::Number(Number::from(value))
                    }
                    opentelemetry::Value::F64(value) => serde_json::Value::Number(
                        Number::from_f64(value).unwrap_or(Number::from(0)),
                    ),
                    opentelemetry::Value::String(value) => serde_json::Value::String(value.into()),
                    opentelemetry::Value::Array(value) => match value {
                        opentelemetry::Array::Bool(array) => serde_json::Value::Array(
                            array.into_iter().map(serde_json::Value::Bool).collect(),
                        ),
                        opentelemetry::Array::I64(array) => serde_json::Value::Array(
                            array
                                .into_iter()
                                .map(|value| serde_json::Value::Number(Number::from(value)))
                                .collect(),
                        ),
                        opentelemetry::Array::F64(array) => serde_json::Value::Array(
                            array
                                .into_iter()
                                .map(|value| {
                                    serde_json::Value::Number(
                                        Number::from_f64(value).unwrap_or(Number::from(0)),
                                    )
                                })
                                .collect(),
                        ),
                        opentelemetry::Array::String(array) => serde_json::Value::Array(
                            array
                                .into_iter()
                                .map(|s| serde_json::Value::String(s.to_string()))
                                .collect(),
                        ),
                    },
                },
            )
        })
        .collect()
}

pub(crate) trait EventFormatter<S> {
    fn format_event<W>(
        &self,
        ctx: &Context<'_, S>,
        writer: &mut W,
        event: &tracing::Event<'_>,
    ) -> fmt::Result
    where
        W: std::fmt::Write;
}
