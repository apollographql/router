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
                            0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0,
                        ]),
                        aggregation::stateless_temporality_selector(),
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
