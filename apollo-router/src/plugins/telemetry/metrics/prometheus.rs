use std::sync::Arc;
use std::sync::Weak;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use futures::future::BoxFuture;
use http::StatusCode;
use opentelemetry_prometheus::PrometheusExporter;
use opentelemetry_prometheus::ResourceSelector;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::Pipeline;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::reader::MetricReader;
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
use crate::plugins::telemetry::metrics::OverflowTracker;
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

        let exporter = SharedPrometheusExporter::from(
            opentelemetry_prometheus::exporter()
                .with_resource_selector(self.resource_selector)
                .with_registry(registry.clone())
                .build()?,
        );

        builder.with_reader(MeterProviderType::Public, exporter.clone());
        // Scrapes bypass reader wrappers, so the endpoint checks each scrape for overflow itself
        builder.with_prometheus_registry(PrometheusRegistry {
            registry,
            exporter,
            overflow_tracker: Some(OverflowTracker::default()),
        });

        Ok(())
    }
}

/// The Prometheus exporter, shared by the public meter provider, which reads from it, and the
/// endpoint, which collects from it directly to name overflowing metrics.
#[derive(Clone, Debug)]
pub(crate) struct SharedPrometheusExporter(Arc<PrometheusExporter>);

impl From<PrometheusExporter> for SharedPrometheusExporter {
    fn from(exporter: PrometheusExporter) -> Self {
        Self(Arc::new(exporter))
    }
}

impl MetricReader for SharedPrometheusExporter {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.0.register_pipeline(pipeline)
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        self.0.collect(rm)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.0.temporality(kind)
    }
}

/// What the Prometheus endpoint serves scrapes from: the metrics registry, the exporter behind
/// it, and the tracker that scrapes use to count cardinality overflow.
#[derive(Clone, Debug)]
pub(crate) struct PrometheusRegistry {
    /// The metrics each scrape gathers.
    pub(crate) registry: Registry,
    /// The exporter that `registry` gathers from. Collecting from it directly gives the same
    /// metrics with their OpenTelemetry names.
    pub(crate) exporter: SharedPrometheusExporter,
    /// Counts cardinality overflow on each scrape. Present when Prometheus is the public meter
    /// provider's only exporter; `None` when a push exporter counts instead.
    pub(crate) overflow_tracker: Option<OverflowTracker>,
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
        // Endpoint services are buffered, so `call` runs on the buffer worker task. Doing the work
        // in the response future keeps the overflow counter on the task handling the scrape. That
        // only matters for task-local test meter providers (`with_metrics`); in production the
        // counter goes to the global meter provider whichever task records it.
        Box::pin(async move {
            // As with the push exporters, the counter shows up from the next collection.
            let metric_families = match &registry.overflow_tracker {
                Some(overflow_tracker) => overflow_tracker.gather_and_record(
                    || registry.registry.gather(),
                    |metrics| registry.exporter.collect(metrics),
                ),
                None => registry.registry.gather(),
            };
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
