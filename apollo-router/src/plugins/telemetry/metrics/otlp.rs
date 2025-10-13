use http::Uri;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_otlp::WithTonicConfig;
use opentelemetry_sdk::metrics::Instrument;
use opentelemetry_sdk::metrics::PeriodicReader;
use opentelemetry_sdk::metrics::StreamBuilder;
use tonic::metadata::MetadataMap;
use tower::BoxError;

use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::otlp::process_endpoint;
use crate::plugins::telemetry::otlp::Protocol;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

impl MetricsConfigurator for super::super::otlp::Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        mut builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        if !self.enabled {
            return Ok(builder);
        }
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
                let endpoint_opt =
                    process_endpoint(&self.endpoint, &kind, &self.protocol)?;
                let headers = self.http.headers.clone();
                let mut exporter = opentelemetry_otlp::HttpExporterBuilder::default()
                    .with_protocol(opentelemetry_otlp::Protocol::Grpc)
                    .with_timeout(self.batch_processor.max_export_timeout)
                    .with_headers(headers);
                if let Some(endpoint) = endpoint_opt {
                    exporter = exporter.with_endpoint(endpoint);
                }
                let temporality = match self.temporality {
                    crate::plugins::telemetry::otlp::Temporality::Cumulative => opentelemetry_sdk::metrics::Temporality::Cumulative,
                    crate::plugins::telemetry::otlp::Temporality::Delta => opentelemetry_sdk::metrics::Temporality::Delta,
                };
                exporter.build_metrics_exporter(temporality)?
            }
        };

        builder.public_meter_provider_builder = builder.public_meter_provider_builder.with_reader(
            PeriodicReader::builder(exporter)
                .with_interval(self.batch_processor.scheduled_delay)
                .build(),
        );
        for metric_view in metrics_config.views.clone() {
            let view = move |i: &Instrument| {
                let stream_builder: Result<StreamBuilder, String> = metric_view.clone().try_into();
                if i.name() == metric_view.name {
                    match stream_builder {
                        Ok(stream_builder) => stream_builder.build().ok(),
                        Err(_) => None,
                    }
                } else {
                    None
                }
            };
            builder.public_meter_provider_builder =
                builder.public_meter_provider_builder.with_view(view);
        }
        Ok(builder)
    }
}
