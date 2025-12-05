use opentelemetry_sdk::metrics::PeriodicReader;
use tower::BoxError;

use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::error_handler::NamedMetricsExporter;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
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
        let exporter = self.metric_exporter(TelemetryDataKind::Metrics)?;
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
