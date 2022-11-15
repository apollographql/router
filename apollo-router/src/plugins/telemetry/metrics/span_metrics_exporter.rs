use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
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

#[derive(Debug, Default)]
pub(crate) struct Exporter {}
#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans metrics to real metrics
    fn export(&mut self, batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        for span in batch
            .into_iter()
            .filter(|s| SPAN_NAMES.contains(&s.name.as_ref()))
        {
            let busy = span
                .attributes
                .get(&Key::from_static_str("busy_ns"))
                .and_then(|attr| match attr {
                    Value::I64(v) => Some(*v),
                    _ => None,
                })
                .unwrap_or_default();
            let idle = span
                .attributes
                .get(&Key::from_static_str("idle_ns"))
                .and_then(|attr| match attr {
                    Value::I64(v) => Some(*v),
                    _ => None,
                })
                .unwrap_or_default();
            let duration = span
                .end_time
                .duration_since(span.start_time)
                .unwrap_or_default()
                .as_millis() as f64;

            let idle_ms: f64 = idle as f64 / 1000000_f64;
            let busy_ms: f64 = busy as f64 / 1000000_f64;
            ::tracing::info!(histogram.apollo_router_span = duration, kind = %"duration", span = %span.name);
            ::tracing::info!(histogram.apollo_router_span = idle_ms, kind = %"idle_ms", span = %span.name);
            ::tracing::info!(histogram.apollo_router_span = busy_ms, kind = %"busy_ms", span = %span.name);
        }

        async { Ok(()) }.boxed()
    }
}
