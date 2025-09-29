//! Configuration for Otlp tracing.
use std::result::Result;

// use opentelemetry_otlp::HttpExporterBuilder;
// use opentelemetry_otlp::TonicExporterBuilder;
// use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_sdk::trace::Builder;
use opentelemetry_stdout::MetricExporterBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
// use crate::plugins::telemetry::otel::named_runtime_channel::NamedTokioRuntime;
// use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

impl TracingConfigurator for super::super::otlp::Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        _builder: Builder,
        _common: &TracingCommon,
        _spans_config: &Spans,
    ) -> Result<Builder, BoxError> {
        todo!("We can uncomment this when SpanExporterBuilder is re-exported in v0.30.0")
    //     let exporter_builder: SpanExporterBuilder = self.exporter(TelemetryDataKind::Traces)?;
    //     let exporter: SpanExporter = exporter_builder.build_span_exporter()?;
    //     let batch_span_processor = BatchSpanProcessor::builder(
    //         exporter,
    //         NamedTokioRuntime::new("otlp-tracing"),
    //     )
    //     .with_batch_config(self.batch_processor.clone().into())
    //     .build()
    //     .filtered();
    //     Ok(
    //         if common.preview_datadog_agent_sampling.unwrap_or_default() {
    //             builder.with_span_processor(batch_span_processor.always_sampled())
    //         } else {
    //             builder.with_span_processor(batch_span_processor)
    //         },
    //     )
    }
}
