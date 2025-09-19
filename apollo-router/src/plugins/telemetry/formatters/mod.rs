//! Our formatters and visitors used for logging
pub(crate) mod json;
pub(crate) mod text;

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

use opentelemetry::KeyValue;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceId;
use opentelemetry_sdk::Resource;
use parking_lot::Mutex;
use serde_json::Number;
use tracing::Subscriber;
use tracing_core::callsite::Identifier;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use super::config_new::logging::RateLimit;
use super::dynamic_attribute::LogAttributes;
use super::reload::SampledSpan;
use crate::plugins::telemetry::otel::OtelData;

pub(crate) const APOLLO_PRIVATE_PREFIX: &str = "apollo_private.";
// FIXME: this is a temporary solution to avoid exposing hardcoded attributes in connector spans instead of using the custom telemetry features.
// The reason this is introduced right now is to directly avoid people relying on these attributes and then creating a breaking change in the future.
pub(crate) const APOLLO_CONNECTOR_PREFIX: &str = "apollo.connector.";
// This list comes from Otel https://opentelemetry.io/docs/specs/semconv/attributes-registry/code/ and
pub(crate) const EXCLUDED_ATTRIBUTES: [&str; 5] = [
    "code.filepath",
    "code.namespace",
    "code.lineno",
    "thread.id",
    "thread.name",
];

/// Wrap a [tracing] event formatter with rate limiting.
///
/// ```ignore
/// use tracing_core::Event;
/// use tracing_subscriber::fmt::format::Format;
/// use crate::plugins::telemetry::config_new::logging::RateLimit;
///
/// tracing_subscriber::fmt::fmt()
///     .event_format(RateLimitFormatter::new(
///         Format::default().pretty(),
///         &RateLimit::default(),
///     ))
///     .finish();
/// ```
pub(crate) struct RateLimitFormatter<T> {
    inner: T,
    rate_limiter: Mutex<HashMap<Identifier, RateCounter>>,
    config: RateLimit,
}

impl<T> RateLimitFormatter<T> {
    pub(crate) fn new(inner: T, rate_limit: &RateLimit) -> Self {
        Self {
            inner,
            rate_limiter: Mutex::new(HashMap::new()),
            config: rate_limit.clone(),
        }
    }
}

impl<T, S, N> FormatEvent<S, N> for RateLimitFormatter<T>
where
    T: FormatEvent<S, N>,
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
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
                            attributes.insert(KeyValue::new("skipped_messages", skipped as i64));
                            extensions.insert(attributes);
                        }
                        Some(attributes) => {
                            attributes.insert(KeyValue::new("skipped_messages", skipped as i64));
                        }
                    }
                }
            }
        }
        self.inner.format_event(ctx, writer, event)
    }
}

impl<T, S> EventFormatter<S> for RateLimitFormatter<T>
where
    T: EventFormatter<S>,
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
                            attributes.insert(KeyValue::new("skipped_messages", skipped as i64));
                            extensions.insert(attributes);
                        }
                        Some(attributes) => {
                            attributes.insert(KeyValue::new("skipped_messages", skipped as i64));
                        }
                    }
                }
            }
        }
        self.inner.format_event(ctx, writer, event)
    }
}

enum RateResult {
    Allow,
    AllowSkipped(u32),
    Deny,
}
impl<T> RateLimitFormatter<T> {
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

pub(crate) fn to_list(resource: Resource) -> Vec<(String, serde_json::Value)> {
    resource
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                match v {
                    opentelemetry::Value::Bool(value) => serde_json::Value::Bool(*value),
                    opentelemetry::Value::I64(value) => {
                        serde_json::Value::Number(Number::from(*value))
                    }
                    opentelemetry::Value::F64(value) => serde_json::Value::Number(
                        Number::from_f64(*value).unwrap_or(Number::from(0)),
                    ),
                    opentelemetry::Value::String(value) => {
                        serde_json::Value::String(value.to_string())
                    }
                    opentelemetry::Value::Array(value) => match value {
                        opentelemetry::Array::Bool(array) => serde_json::Value::Array(
                            array.iter().copied().map(serde_json::Value::Bool).collect(),
                        ),
                        opentelemetry::Array::I64(array) => serde_json::Value::Array(
                            array
                                .iter()
                                .map(|value| serde_json::Value::Number(Number::from(*value)))
                                .collect(),
                        ),
                        opentelemetry::Array::F64(array) => serde_json::Value::Array(
                            array
                                .iter()
                                .map(|value| {
                                    serde_json::Value::Number(
                                        Number::from_f64(*value).unwrap_or(Number::from(0)),
                                    )
                                })
                                .collect(),
                        ),
                        opentelemetry::Array::String(array) => serde_json::Value::Array(
                            array
                                .iter()
                                .map(|s| serde_json::Value::String(s.to_string()))
                                .collect(),
                        ),
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
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
    if let Some(sampled_span) = ext.get::<SampledSpan>() {
        let (trace_id, span_id) = sampled_span.trace_and_span_id();
        return Some((
            opentelemetry::trace::TraceId::from(trace_id.to_u128()),
            span_id,
        ));
    }

    None
}
