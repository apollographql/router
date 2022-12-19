//! Configuration for Otlp tracing.
use std::result::Result;

use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use opentelemetry_otlp::SpanExporterBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for super::super::otlp::Config {
    fn apply(&self, builder: Builder, _trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::info!("configuring Otlp tracing: {}", self.batch_processor);
        let exporter: SpanExporterBuilder = self.exporter()?;
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
