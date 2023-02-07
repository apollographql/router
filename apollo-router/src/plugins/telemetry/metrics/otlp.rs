use opentelemetry::sdk::export::metrics::aggregation;
use opentelemetry::sdk::metrics::selectors;
use opentelemetry::sdk::Resource;
use opentelemetry::KeyValue;
use opentelemetry_otlp::HttpExporterBuilder;
use opentelemetry_otlp::TonicExporterBuilder;
use tower::BoxError;

use crate::plugins::telemetry::config::MetricsCommon;
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
    fn apply(
        &self,
        mut builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        let exporter: MetricExporterBuilder = self.exporter()?;
        match exporter.exporter {
            Some(exporter) => {
                let exporter = opentelemetry_otlp::new_pipeline()
                    .metrics(
                        selectors::simple::histogram([
                            // 1-500 ms
                            1.0,
                            2.0,
                            5.0,
                            10.0,
                            20.0,
                            50.0,
                            100.0,
                            200.0,
                            500.0,
                            // 1-500 seconds
                            1_000.0,
                            2000.0,
                            5_000.0,
                            10_000.0,
                            20_000.0,
                            50_000.0,
                            100_000.0,
                            200_000.0,
                            500_000.0,
                            // 1000-5000 seconds
                            1_000_000.0,
                            2_000_000.0,
                            5_000_000.0,
                        ]),
                        aggregation::delta_temporality_selector(),
                        opentelemetry::runtime::Tokio,
                    )
                    .with_exporter(exporter)
                    .with_resource(Resource::new(
                        metrics_config
                            .resources
                            .clone()
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    ))
                    .build()?;
                builder = builder.with_meter_provider(exporter.clone());
                builder = builder.with_exporter(exporter);
                Ok(builder)
            }
            None => Err("otlp metric export does not support http yet".into()),
        }
    }
}
