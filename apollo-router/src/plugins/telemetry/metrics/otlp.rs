use opentelemetry::runtime;
use opentelemetry::sdk::metrics::PeriodicReader;
use opentelemetry::sdk::metrics::View;
use opentelemetry_otlp::MetricsExporterBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::CustomAggregationSelector;
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
        let exporter_builder: MetricsExporterBuilder = self.exporter(TelemetryDataKind::Metrics)?;
        let exporter = exporter_builder.build_metrics_exporter(
            (&self.temporality).into(),
            Box::new(
                CustomAggregationSelector::builder()
                    .boundaries(metrics_config.buckets.clone())
                    .build(),
            ),
        )?;

        builder.public_meter_provider_builder = builder.public_meter_provider_builder.with_reader(
            PeriodicReader::builder(exporter, runtime::Tokio)
                .with_interval(self.batch_processor.scheduled_delay)
                .with_timeout(self.batch_processor.max_export_timeout)
                .build(),
        );
        for metric_view in metrics_config.views.clone() {
            let view: Box<dyn View> = metric_view.try_into()?;
            builder.public_meter_provider_builder =
                builder.public_meter_provider_builder.with_view(view);
        }
        Ok(builder)
    }
}
