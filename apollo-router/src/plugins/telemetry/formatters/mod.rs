//! Our formatters and visitors used for logging
pub(crate) mod json;
pub(crate) mod text;

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

use opentelemetry::sdk::Resource;
use opentelemetry_api::trace::SpanId;
use opentelemetry_api::trace::TraceContextExt;
use opentelemetry_api::trace::TraceId;
use opentelemetry_api::KeyValue;
use parking_lot::Mutex;
use serde_json::Number;
use tracing::Subscriber;
use tracing_core::callsite::Identifier;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use super::config_new::logging::RateLimit;
use super::dynamic_attribute::LogAttributes;
use crate::metrics::layer::METRIC_PREFIX_COUNTER;
use crate::metrics::layer::METRIC_PREFIX_HISTOGRAM;
use crate::metrics::layer::METRIC_PREFIX_MONOTONIC_COUNTER;
use crate::metrics::layer::METRIC_PREFIX_VALUE;
use crate::plugins::telemetry::otel::OtelData;

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
    rate_limiter: Mutex<HashMap<Identifier, RateCounter>>,
    config: RateLimit,
}

impl<T, F> FilteringFormatter<T, F>
where
    F: Fn(&tracing::Event<'_>) -> bool,
{
    pub(crate) fn new(inner: T, filter_fn: F, rate_limit: &RateLimit) -> Self {
        Self {
            inner,
            filter_fn,
            rate_limiter: Mutex::new(HashMap::new()),
            config: rate_limit.clone(),
        }
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
            match self.rate_limit(event) {
                RateResult::Deny => return Ok(()),

                RateResult::Allow => {}
                RateResult::AllowSkipped(skipped) => {
                    if let Some(span) = event
                        .parent()
                        .and_then(|id| ctx.span(id))
                        .or_else(|| ctx.lookup_current())
                    {
                        let mut extensions = span.extensions_mut();
                        match extensions.get_mut::<LogAttributes>() {
                            None => {
                                let mut attributes = LogAttributes::default();
                                attributes
                                    .insert(KeyValue::new("skipped_messages", skipped as i64));
                                extensions.insert(attributes);
                            }
                            Some(attributes) => {
                                attributes
                                    .insert(KeyValue::new("skipped_messages", skipped as i64));
                            }
                        }
                    }
                }
            }
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
            match self.rate_limit(event) {
                RateResult::Deny => return Ok(()),

                RateResult::Allow => {}
                RateResult::AllowSkipped(skipped) => {
                    if let Some(span) = event
                        .parent()
                        .and_then(|id| ctx.span(id))
                        .or_else(|| ctx.lookup_current())
                    {
                        let mut extensions = span.extensions_mut();
                        match extensions.get_mut::<LogAttributes>() {
                            None => {
                                let mut attributes = LogAttributes::default();
                                attributes
                                    .insert(KeyValue::new("skipped_messages", skipped as i64));
                                extensions.insert(attributes);
                            }
                            Some(attributes) => {
                                attributes
                                    .insert(KeyValue::new("skipped_messages", skipped as i64));
                            }
                        }
                    }
                }
            }
            self.inner.format_event(ctx, writer, event)
        } else {
            Ok(())
        }
    }
}

enum RateResult {
    Allow,
    AllowSkipped(u32),
    Deny,
}
impl<T, F> FilteringFormatter<T, F> {
    fn rate_limit(&self, event: &tracing::Event<'_>) -> RateResult {
        if self.config.enabled {
            let now = Instant::now();
            if let Some(counter) = self
                .rate_limiter
                .lock()
                .get_mut(&event.metadata().callsite())
            {
                if now - counter.last < self.config.interval {
                    counter.count += 1;

                    if counter.count >= self.config.capacity {
                        return RateResult::Deny;
                    }
                } else {
                    if counter.count > self.config.capacity {
                        let skipped = counter.count - self.config.capacity;
                        counter.last = now;
                        counter.count += 1;

                        return RateResult::AllowSkipped(skipped);
                    }

                    counter.last = now;
                    counter.count += 1;
                }

                return RateResult::Allow;
            }

            // this is racy but not a very large issue, we can accept an initial burst
            self.rate_limiter.lock().insert(
                event.metadata().callsite(),
                RateCounter {
                    last: now,
                    count: 1,
                },
            );
        }

        RateResult::Allow
    }
}

struct RateCounter {
    last: Instant,
    count: u32,
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

pub(crate) fn to_list(resource: Resource) -> Vec<(String, serde_json::Value)> {
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

#[inline]
pub(crate) fn get_trace_and_span_id<S>(span: &SpanRef<S>) -> Option<(TraceId, SpanId)>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    let ext = span.extensions();
    if let Some(otel_data) = ext.get::<OtelData>() {
        // The root span is being built and has no parent
        if let (Some(trace_id), Some(span_id)) =
            (otel_data.builder.trace_id, otel_data.builder.span_id)
        {
            return Some((trace_id, span_id));
        }

        // Child spans with a valid trace context
        let span = otel_data.parent_cx.span();
        let span_context = span.span_context();
        if span_context.is_valid() {
            return Some((span_context.trace_id(), span_context.span_id()));
        }
    }
    None
}
