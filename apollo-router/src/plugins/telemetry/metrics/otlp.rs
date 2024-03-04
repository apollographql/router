use opentelemetry::runtime;
use opentelemetry::sdk::metrics::PeriodicReader;
use opentelemetry::sdk::metrics::View;
use opentelemetry_otlp::HttpExporterBuilder;
use opentelemetry_otlp::MetricsExporterBuilder;
use opentelemetry_otlp::TonicExporterBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::CustomAggregationSelector;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;

// TODO Remove MetricExporterBuilder once upstream issue is fixed
// This has to exist because Http is not currently supported for metrics export
// https://github.com/open-telemetry/opentelemetry-rust/issues/772
struct MetricExporterBuilder {
    exporter: Option<TonicExporterBuilder>,
}

impl From<TonicExporterBuilder> for MetricExporterBuilder {
    fn from(exporter: TonicExporterBuilder) -> Self {
        Self {
            exporter: Some(exporter),
        }
    }
}

impl From<HttpExporterBuilder> for MetricExporterBuilder {
    fn from(_exporter: HttpExporterBuilder) -> Self {
        Self { exporter: None }
    }
}

impl MetricsConfigurator for super::super::otlp::Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        mut builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        let exporter: MetricExporterBuilder = self.exporter()?;
        if !self.enabled {
            return Ok(builder);
        }
        match exporter.exporter {
            Some(exporter) => {
                let exporter = MetricsExporterBuilder::Tonic(exporter).build_metrics_exporter(
                    (&self.temporality).into(),
                    Box::new(
                        CustomAggregationSelector::builder()
                            .boundaries(metrics_config.buckets.clone())
                            .build(),
                    ),
                )?;

                builder.public_meter_provider_builder =
                    builder.public_meter_provider_builder.with_reader(
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
            None => Err("otlp metric export does not support http yet".into()),
        }
    }
}
