//! Configuration for Otlp tracing.
use std::result::Result;

use opentelemetry_otlp::SpanExporterBuilder;
use opentelemetry_sdk::trace::BatchSpanProcessor;
use tower::BoxError;

use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::otel::named_runtime_channel::NamedTokioRuntime;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::reload::tracing::TracingBuilder;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;
use crate::plugins::telemetry::tracing::SpanProcessorExt;

impl TracingConfigurator for super::super::otlp::Config {
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.tracing.otlp
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn configure(&self, builder: &mut TracingBuilder) -> Result<(), BoxError> {
        let exporter: SpanExporterBuilder = self.exporter(TelemetryDataKind::Traces)?;
        let batch_span_processor = BatchSpanProcessor::builder(
            exporter.build_span_exporter()?,
            NamedTokioRuntime::new("otlp-tracing"),
        )
        .with_batch_config(self.batch_processor.clone().into())
        .build()
        .filtered();

        if builder
            .tracing_common()
            .preview_datadog_agent_sampling
            .unwrap_or_default()
        {
            builder.with_span_processor(batch_span_processor.always_sampled())
        } else {
            builder.with_span_processor(batch_span_processor)
        }

        Ok(())
    }
}
