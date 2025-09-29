use opentelemetry_otlp::MetricExporter;
use opentelemetry_sdk::metrics::Instrument;
use opentelemetry_sdk::metrics::PeriodicReader;
// use opentelemetry_sdk::metrics::View;
// use opentelemetry_sdk::runtime;
// use opentelemetry_sdk::metrics::Stream;
// use opentelemetry_sdk::metrics::Aggregation;
use tower::BoxError;

use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
// use crate::plugins::telemetry::otlp::TelemetryDataKind;

impl MetricsConfigurator for super::super::otlp::Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        mut _builder: MetricsBuilder,
        _metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        todo!("We can uncomment this when MetricExporterBuilder is re-exported in v0.30.0");
        // if !self.enabled {
        //     return Ok(builder);
        // }
        // let exporter_builder: MetricExporterBuilder  = self.exporter(TelemetryDataKind::Metrics)?;
        // let exporter: MetricExporter = exporter_builder.build_metrics_exporter((&self.temporality));

        // let view_aggregation = |_i: &Instrument| {
        //     Some(
        //         Stream::new() // change to StreamBuilder when we upgrade to v0.30.0
        //             .aggregation(Aggregation::ExplicitBucketHistogram {
        //                 boundaries: metrics_config.buckets.clone(),
        //                 record_min_max: true,
        //             }),
        //     )
        // };

        // builder.public_meter_provider_builder = builder
        //     .public_meter_provider_builder
        //     .with_reader(
        //         PeriodicReader::builder(exporter, runtime::Tokio)
        //             .with_interval(self.batch_processor.scheduled_delay)
        //             .with_timeout(self.batch_processor.max_export_timeout)
        //             .build(),
        //     )
        //     .with_view(view_aggregation);
        // for metric_view in metrics_config.views.clone() {
        //     let view: Box<dyn View> = metric_view.try_into()?;
        //     builder.public_meter_provider_builder =
        //         builder.public_meter_provider_builder.with_view(view);
        // }
        // Ok(builder)
    }
}
