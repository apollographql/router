//! Configuration for Otlp tracing.
use std::result::Result;

use http::Uri;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_otlp::WithTonicConfig;
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
use tonic::metadata::MetadataMap;
use tower::BoxError;

use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::otlp::Config;
use crate::plugins::telemetry::otlp::Protocol;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::otlp::process_endpoint;
use crate::plugins::telemetry::reload::tracing::TracingBuilder;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;
use crate::plugins::telemetry::tracing::NamedSpanExporter;
use crate::plugins::telemetry::tracing::NamedTokioRuntime;
use crate::plugins::telemetry::tracing::SpanProcessorExt;

impl TracingConfigurator for super::super::otlp::Config {
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.tracing.otlp
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn configure(&self, builder: &mut TracingBuilder) -> Result<(), BoxError> {
        // Apply env var overrides to the config
        let config = self.clone().with_tracing_env_overrides()?;

        let exporter = config.build_span_exporter()?;
        let named_exporter = NamedSpanExporter::new(exporter, "otlp");
        let batch_span_processor =
            BatchSpanProcessor::builder(named_exporter, NamedTokioRuntime::new("otlp-tracing"))
                .with_batch_config(config.batch_processor.clone().with_env_overrides()?.into())
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

impl Config {
    pub(crate) fn build_span_exporter(&self) -> Result<opentelemetry_otlp::SpanExporter, BoxError> {
        match self.protocol {
            Protocol::Grpc => self.build_grpc_span_exporter(),
            Protocol::Http => self.build_http_span_exporter(),
        }
    }

    fn build_grpc_span_exporter(&self) -> Result<opentelemetry_otlp::SpanExporter, BoxError> {
        let endpoint_opt =
            process_endpoint(&self.endpoint, &TelemetryDataKind::Traces, &self.protocol)?;
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
            .with_timeout(self.batch_processor.max_export_timeout)
            .with_metadata(MetadataMap::from_headers(self.grpc.metadata.clone()));

        if let Some(endpoint) = endpoint_opt {
            exporter_builder = exporter_builder.with_endpoint(endpoint);
        }
        if let Some(tls_config) = tls_config_opt {
            exporter_builder = exporter_builder.with_tls_config(tls_config);
        }
        Ok(exporter_builder.build()?)
    }

    fn build_http_span_exporter(&self) -> Result<opentelemetry_otlp::SpanExporter, BoxError> {
        let endpoint_opt =
            process_endpoint(&self.endpoint, &TelemetryDataKind::Traces, &self.protocol)?;

        let mut exporter_builder = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_timeout(self.batch_processor.max_export_timeout)
            .with_headers(self.http.headers.clone());

        if let Some(endpoint) = endpoint_opt {
            exporter_builder = exporter_builder.with_endpoint(endpoint);
        }
        Ok(exporter_builder.build()?)
    }
}
