use opentelemetry_otlp::MetricExporter;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
use opentelemetry_sdk::runtime;
use tower::BoxError;

use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::metrics::NamedMetricExporter;
use crate::plugins::telemetry::metrics::OverflowMetricExporter;
use crate::plugins::telemetry::metrics::RetryMetricExporter;
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
        // Apply env var overrides to the config
        let config = self.clone().with_metrics_env_overrides()?;

        let exporter = config.build_metric_exporter()?;

        // // Wrap with retry, then overflow detection, then error prefixing
        let named_exporter = NamedMetricExporter::new(
            OverflowMetricExporter::new_push(RetryMetricExporter::new(exporter)),
            "otlp",
        );
        builder.with_reader(
            MeterProviderType::Public,
            PeriodicReader::builder(named_exporter, runtime::Tokio)
                .with_interval(config.batch_processor.scheduled_delay)
                .build(),
        );

        Ok(())
    }
}

impl super::super::otlp::Config {
    fn build_metric_exporter(&self) -> Result<MetricExporter, BoxError> {
        match self.protocol {
            Protocol::Grpc => self.build_grpc_metric_exporter(),
            Protocol::Http => self.build_http_metric_exporter(),
        }
    }

    fn build_grpc_metric_exporter(&self) -> Result<MetricExporter, BoxError> {
        use http::Uri;
        use opentelemetry_otlp::WithTonicConfig;
        use tonic::metadata::MetadataMap;

        let endpoint_opt =
            process_endpoint(&self.endpoint, &TelemetryDataKind::Metrics, &self.protocol)?;
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

        let mut exporter_builder = MetricExporter::builder()
            .with_tonic()
            .with_temporality(self.temporality.into())
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

    fn build_http_metric_exporter(&self) -> Result<MetricExporter, BoxError> {
        use opentelemetry_otlp::WithHttpConfig;

        let endpoint_opt =
            process_endpoint(&self.endpoint, &TelemetryDataKind::Metrics, &self.protocol)?;

        let mut exporter_builder = MetricExporter::builder()
            .with_http()
            .with_temporality(self.temporality.into())
            .with_timeout(self.batch_processor.max_export_timeout)
            .with_headers(self.http.headers.clone());

        if let Some(endpoint) = endpoint_opt {
            exporter_builder = exporter_builder.with_endpoint(endpoint);
        }

        Ok(exporter_builder.build()?)
    }
}
