use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::{MetricsBuilder, MetricsConfigurator};
use futures::{Stream, StreamExt};
use opentelemetry::sdk::metrics::selectors;
use opentelemetry::util::tokio_interval_stream;
use opentelemetry_otlp::{HttpExporterBuilder, TonicExporterBuilder};
use std::time::Duration;
use tower::BoxError;

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
        _metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        let exporter: MetricExporterBuilder = self.exporter()?;
        match exporter.exporter {
            Some(exporter) => {
                let exporter = opentelemetry_otlp::new_pipeline()
                    .metrics(tokio::spawn, delayed_interval)
                    .with_exporter(exporter)
                    .with_aggregator_selector(selectors::simple::Selector::Exact)
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
