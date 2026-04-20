use axum::Extension;
use axum::response::IntoResponse;
use axum::response::Response;
use http::StatusCode;
use opentelemetry_prometheus::ResourceSelector;
use prometheus::Encoder;
use prometheus::Registry;
use prometheus::TextEncoder;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::ListenAddr;
use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::metrics::OverflowMetricExporter;
use crate::plugins::telemetry::reload::metrics::MetricsBuilder;
use crate::plugins::telemetry::reload::metrics::MetricsConfigurator;

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

        // Wrap with overflow detection to increment cardinality_overflow counter on pull
        let reader = OverflowMetricExporter::new_pull(exporter);
        builder.with_reader(MeterProviderType::Public, reader);
        builder.with_prometheus_registry(registry);

        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct PrometheusState {
    pub(crate) registry: Registry,
}

pub(crate) async fn handle_prometheus(Extension(state): Extension<PrometheusState>) -> Response {
    let metric_families = state.registry.gather();
    let encoder = TextEncoder::new();
    let mut result = Vec::new();
    if let Err(e) = encoder.encode(&metric_families, &mut result) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    // otel 0.19.0 started adding "_total" onto various statistics.
    let stats = String::from_utf8_lossy(&result);
    let modified_stats = stats.replace("_total_total", "_total");

    (
        StatusCode::OK,
        [(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        modified_stats,
    )
        .into_response()
}
