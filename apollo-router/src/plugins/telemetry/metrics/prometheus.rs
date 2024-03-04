use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
use once_cell::sync::Lazy;
use opentelemetry::sdk::metrics::MeterProvider;
use opentelemetry::sdk::metrics::View;
use opentelemetry::sdk::Resource;
use prometheus::Encoder;
use prometheus::Registry;
use prometheus::TextEncoder;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceExt;
use tower_service::Service;

use crate::plugins::telemetry::config::MetricView;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::CustomAggregationSelector;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::router_factory::Endpoint;
use crate::services::router;
use crate::ListenAddr;

/// Prometheus configuration
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Config {
    /// Set to true to enable
    pub(crate) enabled: bool,
    /// The listen address
    pub(crate) listen: ListenAddr,
    /// The path where prometheus will be exposed
    pub(crate) path: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: ListenAddr::SocketAddr("127.0.0.1:9090".parse().expect("valid listenAddr")),
            path: "/metrics".to_string(),
        }
    }
}

// Prometheus metrics are special. We want them to persist between restarts if possible.
// This means reusing the existing registry and meter provider if we can.
// These statics will keep track of new registry for commit when the telemetry plugin is activated.
static EXISTING_PROMETHEUS: Lazy<Mutex<Option<(PrometheusConfig, Registry)>>> =
    Lazy::new(Default::default);
static NEW_PROMETHEUS: Lazy<Mutex<Option<(PrometheusConfig, Registry)>>> =
    Lazy::new(Default::default);

#[derive(PartialEq, Clone)]
struct PrometheusConfig {
    resource: Resource,
    buckets: Vec<f64>,
    views: Vec<MetricView>,
}

pub(crate) fn commit_prometheus() {
    if let Some(prometheus) = NEW_PROMETHEUS.lock().expect("lock poisoned").take() {
        tracing::debug!("committing prometheus registry");
        EXISTING_PROMETHEUS
            .lock()
            .expect("lock poisoned")
            .replace(prometheus);
    }
}

impl MetricsConfigurator for Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        mut builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        // Prometheus metrics are special, they must persist between reloads. This means that we only want to create something new if the resources have changed.
        // The prometheus exporter, and the associated registry are linked, so replacing one means replacing the other.

        let prometheus_config = PrometheusConfig {
            resource: builder.resource.clone(),
            buckets: metrics_config.buckets.clone(),
            views: metrics_config.views.clone(),
        };

        // Check the last registry to see if the resources are the same, if they are we can use it as is.
        // Otherwise go with the new controller and store it so that it can be committed during telemetry activation.
        // Note that during tests the prom registry cannot be reused as we have a different meter provider for each test.
        // Prom reloading IS tested in an integration test.
        #[cfg(not(test))]
        if let Some((last_config, last_registry)) =
            EXISTING_PROMETHEUS.lock().expect("lock poisoned").clone()
        {
            if prometheus_config == last_config {
                tracing::debug!("prometheus registry can be reused");
                builder.custom_endpoints.insert(
                    self.listen.clone(),
                    Endpoint::from_router_service(
                        self.path.clone(),
                        PrometheusService {
                            registry: last_registry.clone(),
                        }
                        .boxed(),
                    ),
                );
                tracing::info!(
                    "Prometheus endpoint exposed at {}{}",
                    self.listen,
                    self.path
                );
                return Ok(builder);
            } else {
                tracing::debug!("prometheus registry cannot be reused");
            }
        }

        let registry = prometheus::Registry::new();

        let exporter = opentelemetry_prometheus::exporter()
            .with_aggregation_selector(
                CustomAggregationSelector::builder()
                    .boundaries(metrics_config.buckets.clone())
                    .record_min_max(true)
                    .build(),
            )
            .with_registry(registry.clone())
            .build()?;

        let mut meter_provider_builder = MeterProvider::builder()
            .with_reader(exporter)
            .with_resource(builder.resource.clone());
        for metric_view in metrics_config.views.clone() {
            let view: Box<dyn View> = metric_view.try_into()?;
            meter_provider_builder = meter_provider_builder.with_view(view);
        }
        let meter_provider = meter_provider_builder.build();
        builder.custom_endpoints.insert(
            self.listen.clone(),
            Endpoint::from_router_service(
                self.path.clone(),
                PrometheusService {
                    registry: registry.clone(),
                }
                .boxed(),
            ),
        );
        builder.prometheus_meter_provider = Some(meter_provider.clone());

        NEW_PROMETHEUS
            .lock()
            .expect("lock poisoned")
            .replace((prometheus_config, registry));

        tracing::info!(
            "Prometheus endpoint exposed at {}{}",
            self.listen,
            self.path
        );

        Ok(builder)
    }
}

#[derive(Clone)]
pub(crate) struct PrometheusService {
    registry: Registry,
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
            Ok(router::Response {
                response: http::Response::builder()
                    .status(StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")
                    .body::<hyper::Body>(modified_stats.into())
                    .map_err(BoxError::from)?,
                context: req.context,
            })
        })
    }
}
