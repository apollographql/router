use http::Uri;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_otlp::WithTonicConfig;
use opentelemetry_sdk::metrics::PeriodicReader;
use tonic::metadata::MetadataMap;
use tower::BoxError;

use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::error_handler::NamedMetricsExporter;
use crate::plugins::telemetry::otlp::Protocol;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::otlp::process_endpoint;
use crate::plugins::telemetry::reload::metrics::MetricsBuilder;
use crate::plugins::telemetry::reload::metrics::MetricsConfigurator;

impl MetricsConfigurator for super::super::otlp::Config {
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.metrics.otlp
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn configure(&self, builder: &mut MetricsBuilder) -> Result<(), BoxError> {
        let kind = TelemetryDataKind::Metrics;
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

                let mut exporter_builder = opentelemetry_otlp::MetricExporter::builder()
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
                let temporality = match self.temporality {
                    crate::plugins::telemetry::otlp::Temporality::Cumulative => {
                        opentelemetry_sdk::metrics::Temporality::Cumulative
                    }
                    crate::plugins::telemetry::otlp::Temporality::Delta => {
                        opentelemetry_sdk::metrics::Temporality::Delta
                    }
                };
                exporter.build_metrics_exporter(temporality)?
            }
        };

        let named_exporter = NamedMetricsExporter::new(exporter, "otlp");
        builder.with_reader(
            MeterProviderType::Public,
            PeriodicReader::builder(named_exporter)
                .with_interval(self.batch_processor.scheduled_delay)
                .build(),
        );

        Ok(())
    }
}
