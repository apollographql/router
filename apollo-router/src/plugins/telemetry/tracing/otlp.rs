//! Configuration for Otlp tracing.
use std::result::Result;

use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_sdk::trace::TracerProviderBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::otlp::process_endpoint;
use crate::plugins::telemetry::otlp::Protocol;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for super::super::otlp::Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        builder: TracerProviderBuilder,
        common: &TracingCommon,
        _spans_config: &Spans,
    ) -> Result<TracerProviderBuilder, BoxError> {
        let exporter = match self.protocol {
            // if they are using a TonicExporter, customers will need to configure metadata, timeout and endpoint/tls_config
            // using env variables: OTEL_EXPORTER_OTLP_TRACES_HEADERS (metadata), OTEL_EXPORTER_OTLP_TRACES_TIMEOUT, OTEL_EXPORTER_OTLP_TRACES_ENDPOINT
            Protocol::Grpc => {
                opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .build()?
            }
            Protocol::Http => {
                let endpoint_opt = process_endpoint(&self.endpoint, &TelemetryDataKind::Traces, &self.protocol)?;
                let headers = self.http.headers.clone();
                let mut exporter = opentelemetry_otlp::HttpExporterBuilder::default()
                    .with_protocol(opentelemetry_otlp::Protocol::Grpc)
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with_headers(headers);
                if let Some(endpoint) = endpoint_opt {
                    exporter = exporter.with_endpoint(endpoint);
                }
                exporter.build_span_exporter()?
            }
        };

        let batch_span_processor = BatchSpanProcessor::builder(exporter)
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
