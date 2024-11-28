//! Configuration for Otlp tracing.
use std::result::Result;

use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use opentelemetry_otlp::SpanExporterBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
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
        _common: &TracingCommon,
        _spans_config: &Spans,
    ) -> Result<Builder, BoxError> {
        tracing::info!("Configuring Otlp tracing: {}", self.batch_processor);
        let exporter: SpanExporterBuilder = self.exporter(TelemetryDataKind::Traces)?;

        Ok(builder.with_span_processor(
            BatchSpanProcessor::builder(
                exporter.build_span_exporter()?,
                opentelemetry::runtime::Tokio,
            )
            .with_batch_config(self.batch_processor.clone().into())
            .build()
            .filtered(),
        ))
    }
}
