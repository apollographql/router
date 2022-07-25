use std::time::Duration;

use futures::Stream;
use futures::StreamExt;
use opentelemetry::sdk::metrics::selectors;
use opentelemetry::util::tokio_interval_stream;
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
                    .metrics(tokio::spawn, delayed_interval)
                    .with_exporter(exporter)
                    .with_aggregator_selector(selectors::simple::Selector::Exact)
                    .with_resource(
                        metrics_config
                            .resources
                            .clone()
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    )
                    .build()?;
                builder = builder.with_meter_provider(exporter.provider());
                builder = builder.with_exporter(exporter);
                Ok(builder)
            }
            None => Err("otlp metric export does not support http yet".into()),
        }
    }
}

fn delayed_interval(duration: Duration) -> impl Stream<Item = tokio::time::Instant> {
    tokio_interval_stream(duration).skip(1)
}
