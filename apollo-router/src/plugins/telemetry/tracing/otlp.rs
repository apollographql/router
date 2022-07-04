//! Configuration for Otlp tracing.
use std::result::Result;

use opentelemetry::sdk::trace::Builder;
use opentelemetry_otlp::SpanExporterBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for super::super::otlp::Config {
    fn apply(&self, builder: Builder, _trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Otlp tracing");
        let exporter: SpanExporterBuilder = self.exporter()?;
        Ok(builder.with_batch_exporter(
            exporter.build_span_exporter()?,
            opentelemetry::runtime::Tokio,
        ))
    }
}
