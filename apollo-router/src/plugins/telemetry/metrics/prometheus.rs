use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
use opentelemetry_prometheus::ResourceSelector;
use prometheus::Encoder;
use prometheus::Registry;
use prometheus::TextEncoder;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower_service::Service;

use crate::ListenAddr;
use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::metrics::CustomAggregationSelector;
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

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(&self, builder: &mut MetricsBuilder) -> Result<(), BoxError> {
        let registry = Registry::new();

        let exporter = opentelemetry_prometheus::exporter()
            .with_aggregation_selector(
                CustomAggregationSelector::builder()
                    .boundaries(builder.metrics_common().buckets.clone())
                    .record_min_max(true)
                    .build(),
            )
            .with_resource_selector(self.resource_selector)
            .with_registry(registry.clone())
            .build()?;

        builder.with_reader(MeterProviderType::Public, exporter);
        builder.with_prometheus_registry(registry);

        tracing::info!(
            "Prometheus endpoint exposed at {}{}",
            self.listen,
            self.path
        );

        Ok(())
    }
}

pub(crate) struct PrometheusService {
    pub(crate) registry: Registry,
}

impl Service<router::Request> for PrometheusService {
    type Response = router::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        let metric_families = self.registry.gather();
        Box::pin(async move {
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
