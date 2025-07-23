use opentelemetry_sdk::metrics::PeriodicReader;
use opentelemetry_sdk::metrics::StreamBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
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
        let exporter = opentelemetry_otlp::MetricExporter::builder().with_http().build()?;

        builder.public_meter_provider_builder = builder.public_meter_provider_builder.with_reader(
            PeriodicReader::builder(exporter)
                .with_interval(self.batch_processor.scheduled_delay)
                .with_timeout(self.batch_processor.max_export_timeout)
                .build(),
        );
        for metric_view in metrics_config.views.clone() {
            let stream_builder: StreamBuilder = metric_view.try_into()?;
            builder.public_meter_provider_builder =
                builder.public_meter_provider_builder.with_view(stream_builder);
        }
        Ok(builder)
    }
}
