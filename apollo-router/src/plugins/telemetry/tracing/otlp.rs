//! Configuration for Otlp tracing.
use std::result::Result;

use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_sdk::trace::Builder;
use tower::BoxError;

use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::otel::named_runtime_channel::NamedTokioRuntime;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for super::super::otlp::Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        builder: Builder,
        common: &TracingCommon,
        _spans_config: &Spans,
    ) -> Result<Builder, BoxError> {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build()?;
        let batch_span_processor = BatchSpanProcessor::builder(
            exporter,
            NamedTokioRuntime::new("otlp-tracing"),
        )
        .with_batch_config(self.batch_processor.clone().into())
        .build()
        .filtered();
        Ok(
            if common.preview_datadog_agent_sampling.unwrap_or_default() {
                builder.with_span_processor(batch_span_processor.always_sampled())
            } else {
                builder.with_span_processor(batch_span_processor)
            },
        )
    }
}
