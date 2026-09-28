use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
use opentelemetry_prometheus::PrometheusExporter;
use opentelemetry_prometheus::ResourceSelector;
use prometheus::Encoder;
use prometheus::Registry;
use prometheus::TextEncoder;
use prometheus::proto::MetricFamily;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower_service::Service;

use crate::ListenAddr;
use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::metrics::OverflowMetricReader;
use crate::plugins::telemetry::reload::metrics::MetricsBuilder;
use crate::plugins::telemetry::reload::metrics::MetricsConfigurator;
use crate::services::router;

/// Prometheus configuration
#[derive(Debug, Clone, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "PrometheusMetricsConfig")]
pub(crate) struct Config {
    /// Set to true to enable
    pub(crate) enabled: bool,
    /// resource_selector is used to select which resource to export with every metrics.
    pub(crate) resource_selector: ResourceSelectorConfig,
    /// The listen address
    pub(crate) listen: ListenAddr,
    /// The path where prometheus will be exposed
    pub(crate) path: String,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceSelectorConfig {
    /// Export all resource attributes with every metrics.
    All,
    #[default]
    /// Do not export any resource attributes with every metrics.
    None,
}

impl From<ResourceSelectorConfig> for ResourceSelector {
    fn from(value: ResourceSelectorConfig) -> Self {
        match value {
            ResourceSelectorConfig::All => ResourceSelector::All,
            ResourceSelectorConfig::None => ResourceSelector::None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            resource_selector: ResourceSelectorConfig::default(),
            listen: ListenAddr::SocketAddr("127.0.0.1:9090".parse().expect("valid listenAddr")),
            path: "/metrics".to_string(),
        }
    }
}

impl MetricsConfigurator for Config {
    fn config(conf: &Conf) -> &Self {
        &conf.exporters.metrics.prometheus
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn configure(&self, builder: &mut MetricsBuilder) -> Result<(), BoxError> {
        let registry = Registry::new();

        let exporter = opentelemetry_prometheus::exporter()
            .with_resource_selector(self.resource_selector)
            .with_registry(registry.clone())
            .build()?;

        // Scrapes bypass reader wrappers, so the endpoint keeps a handle to check for overflow itself
        let reader = OverflowMetricReader::new(exporter);
        builder.with_reader(MeterProviderType::Public, reader.clone());
        builder.with_prometheus_registry(PrometheusRegistry {
            registry,
            overflow_reader: Some(reader),
        });

        Ok(())
    }
}

/// The registry backing the Prometheus endpoint, with the reader used to detect cardinality
/// overflow.
#[derive(Clone, Debug)]
pub(crate) struct PrometheusRegistry {
    pub(crate) registry: Registry,
    /// Present when scrapes are responsible for counting cardinality overflow on the public meter
    /// provider. `None` when a push exporter on the same provider already counts it.
    pub(crate) overflow_reader: Option<OverflowMetricReader<PrometheusExporter>>,
}

pub(crate) struct PrometheusService {
    pub(crate) registry: PrometheusRegistry,
}

impl Service<router::Request> for PrometheusService {
    type Response = router::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        let registry = self.registry.clone();
        // Work happens in the response future rather than `call`: endpoint services are buffered,
        // so `call` runs on the buffer worker task rather than the task handling the scrape.
        Box::pin(async move {
            let metric_families = registry.registry.gather();
            if let Some(reader) = &registry.overflow_reader
                && has_overflow(&metric_families)
            {
                // The scrape only carries Prometheus names, so collect again to report
                // OpenTelemetry instrument names. As with the push exporters, the counter shows
                // up from the next collection.
                reader.report_cardinality_overflow();
            }
            let encoder = TextEncoder::new();
            let mut result = Vec::new();
            encoder.encode(&metric_families, &mut result)?;
            // otel 0.19.0 started adding "_total" onto various statistics.
            // Let's remove any problems they may have created for us.
            let stats = String::from_utf8_lossy(&result);
            let modified_stats = stats.replace("_total_total", "_total");

            router::Response::http_response_builder()
                .response(
                    http::Response::builder()
                        .status(StatusCode::OK)
                        .header(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")
                        .body(router::body::from_bytes(modified_stats))
                        .map_err(BoxError::from)?,
                )
                .context(req.context)
                .build()
        })
    }
}

/// Whether any gathered series carries the SDK's cardinality overflow marker.
fn has_overflow(metric_families: &[MetricFamily]) -> bool {
    metric_families.iter().any(|family| {
        family.get_metric().iter().any(|metric| {
            metric
                .get_label()
                .iter()
                .any(|label| label.name() == "otel_metric_overflow" && label.value() == "true")
        })
    })
}
