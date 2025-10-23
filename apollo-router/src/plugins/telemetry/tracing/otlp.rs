//! Configuration for Otlp tracing.
use std::result::Result;

use http::Uri;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_otlp::WithTonicConfig;
use opentelemetry_sdk::trace::BatchSpanProcessor;
use tonic::metadata::MetadataMap;
use tower::BoxError;

use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::otlp::Protocol;
use crate::plugins::telemetry::error_handler::NamedSpanExporter;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::otlp::process_endpoint;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::reload::tracing::TracingBuilder;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;

impl TracingConfigurator for super::super::otlp::Config {
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.tracing.otlp
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn configure(&self, builder: &mut TracingBuilder) -> Result<(), BoxError> {
        let kind = TelemetryDataKind::Traces;
        let exporter = match self.protocol {
            Protocol::Grpc => {
                let endpoint_opt = process_endpoint(&self.endpoint, &kind, &self.protocol)?;
                // Figure out if we need to set tls config for our exporter
                let tls_config_opt = if let Some(endpoint) = &endpoint_opt {
                    if !endpoint.is_empty() {
                        let tls_url = Uri::try_from(endpoint)?;
                        Some(self.grpc.clone().to_tls_config(&tls_url)?)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let mut exporter_builder = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_protocol(opentelemetry_otlp::Protocol::Grpc)
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with_metadata(MetadataMap::from_headers(self.grpc.metadata.clone()));

                if let Some(endpoint) = endpoint_opt {
                    exporter_builder = exporter_builder.with_endpoint(endpoint);
                }
                if let Some(tls_config) = tls_config_opt {
                    exporter_builder = exporter_builder.with_tls_config(tls_config);
                }

                exporter_builder.build()?
            }
            Protocol::Http => {
                let endpoint_opt = process_endpoint(&self.endpoint, &kind, &self.protocol)?;
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
        let named_exporter = NamedSpanExporter::new(exporter, "otlp");
        let batch_span_processor = BatchSpanProcessor::builder(named_exporter)
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
