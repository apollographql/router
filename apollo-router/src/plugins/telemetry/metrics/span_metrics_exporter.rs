use async_trait::async_trait;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::trace::Span;
use opentelemetry::sdk::trace::SpanProcessor;
use opentelemetry::trace::TraceResult;
use opentelemetry::Context;
use opentelemetry::Key;
use opentelemetry::Value;

use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::SUPERGRAPH_SPAN_NAME;
use crate::services::QUERY_PLANNING_SPAN_NAME;

const SPAN_NAMES: &[&str] = &[
    REQUEST_SPAN_NAME,
    SUPERGRAPH_SPAN_NAME,
    SUBGRAPH_SPAN_NAME,
    QUERY_PLANNING_SPAN_NAME,
    EXECUTION_SPAN_NAME,
];

const BUSY_NS_ATTRIBUTE_NAME: Key = Key::from_static_str("busy_ns");
const IDLE_NS_ATTRIBUTE_NAME: Key = Key::from_static_str("idle_ns");
const SUBGRAPH_ATTRIBUTE_NAME: Key = Key::from_static_str("apollo.subgraph.name");

#[derive(Debug, Default)]
pub(crate) struct Processor {}
#[async_trait]
impl SpanProcessor for Processor {
    fn on_start(&self, _span: &mut Span, _cx: &Context) {}
    /// Export spans metrics to real metrics
    fn on_end(&self, span: SpanData) {
        if SPAN_NAMES.contains(&span.name.as_ref()) {
            let busy = span
                .attributes
                .get(&BUSY_NS_ATTRIBUTE_NAME)
                .and_then(|attr| match attr {
                    Value::I64(v) => Some(*v),
                    _ => None,
                })
                .unwrap_or_default();
            let idle = span
                .attributes
                .get(&IDLE_NS_ATTRIBUTE_NAME)
                .and_then(|attr| match attr {
                    Value::I64(v) => Some(*v),
                    _ => None,
                })
                .unwrap_or_default();
            let duration = span
                .end_time
                .duration_since(span.start_time)
                .unwrap_or_default()
                .as_secs_f64();

            // Convert it in seconds
            let idle: f64 = idle as f64 / 1_000_000_000_f64;
            let busy: f64 = busy as f64 / 1_000_000_000_f64;
            if span.name == SUBGRAPH_SPAN_NAME {
                let subgraph_name = span
                    .attributes
                    .get(&SUBGRAPH_ATTRIBUTE_NAME)
                    .map(|name| name.as_str())
                    .unwrap_or_default();
                ::tracing::info!(histogram.apollo_router_span = duration, kind = %"duration", span = %span.name, subgraph = %subgraph_name);
                ::tracing::info!(histogram.apollo_router_span = idle, kind = %"idle", span = %span.name, subgraph = %subgraph_name);
                ::tracing::info!(histogram.apollo_router_span = busy, kind = %"busy", span = %span.name, subgraph = %subgraph_name);
            } else {
                ::tracing::info!(histogram.apollo_router_span = duration, kind = %"duration", span = %span.name);
                ::tracing::info!(histogram.apollo_router_span = idle, kind = %"idle", span = %span.name);
                ::tracing::info!(histogram.apollo_router_span = busy, kind = %"busy", span = %span.name);
            }
        }
    }

    fn force_flush(&self) -> TraceResult<()> {
        Ok(())
    }

    fn shutdown(&mut self) -> TraceResult<()> {
        Ok(())
    }
}
