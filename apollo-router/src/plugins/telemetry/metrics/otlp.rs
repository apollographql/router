use opentelemetry_otlp::MetricsExporterBuilder;
use opentelemetry_sdk::metrics::PeriodicReader;
use opentelemetry_sdk::runtime;
use tower::BoxError;

use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::metrics::CustomAggregationSelector;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::reload::metrics::MetricsBuilder;
use crate::plugins::telemetry::reload::metrics::MetricsConfigurator;

impl MetricsConfigurator for super::super::otlp::Config {
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.metrics.otlp
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(&self, builder: &mut MetricsBuilder) -> Result<(), BoxError> {
        let exporter_builder: MetricsExporterBuilder = self.exporter(TelemetryDataKind::Metrics)?;
        let exporter = exporter_builder.build_metrics_exporter(
            (&self.temporality).into(),
            Box::new(
                CustomAggregationSelector::builder()
                    .boundaries(builder.metrics_common().buckets.clone())
                    .build(),
            ),
        )?;

        builder.with_reader(
            MeterProviderType::Public,
            PeriodicReader::builder(exporter, runtime::Tokio)
                .with_interval(self.batch_processor.scheduled_delay)
                .with_timeout(self.batch_processor.max_export_timeout)
                .build(),
        );

        Ok(())
    }
}
