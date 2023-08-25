use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
use once_cell::sync::Lazy;
use opentelemetry::sdk::export::metrics::aggregation;
use opentelemetry::sdk::metrics::controllers;
use opentelemetry::sdk::metrics::controllers::BasicController;
use opentelemetry::sdk::metrics::processors;
use opentelemetry::sdk::metrics::selectors;
use opentelemetry::sdk::Resource;
use opentelemetry::KeyValue;
use prometheus::Encoder;
use prometheus::Registry;
use prometheus::TextEncoder;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceExt;
use tower_service::Service;

use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::filter::FilterMeterProvider;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::router_factory::Endpoint;
use crate::services::router;
use crate::ListenAddr;

/// Prometheus configuration
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Set to true to enable
    pub(crate) enabled: bool,
    /// The listen address
    #[serde(default = "prometheus_default_listen_addr")]
    pub(crate) listen: ListenAddr,
    /// The path where prometheus will be exposed
    #[serde(default = "prometheus_default_path")]
    pub(crate) path: String,
}

fn prometheus_default_listen_addr() -> ListenAddr {
    ListenAddr::SocketAddr("127.0.0.1:9090".parse().expect("valid listenAddr"))
}

fn prometheus_default_path() -> String {
    "/metrics".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: prometheus_default_listen_addr(),
            path: prometheus_default_path(),
        }
    }
}

// Prometheus metrics are special. We want them to persist between restarts if possible.
// This means reusing the existing controller if we can.
// These statics will keep track of new controllers for commit when the telemetry plugin is activated.
static CONTROLLER: Lazy<Mutex<Option<BasicController>>> = Lazy::new(Default::default);
static NEW_CONTROLLER: Lazy<Mutex<Option<BasicController>>> = Lazy::new(Default::default);

pub(crate) fn commit_new_controller() {
    if let Some(controller) = NEW_CONTROLLER.lock().expect("lock poisoned").take() {
        tracing::debug!("committing prometheus controller");
        CONTROLLER
            .lock()
            .expect("lock poisoned")
            .replace(controller);
    }
}

impl MetricsConfigurator for Config {
    fn apply(
        &self,
        mut builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        if self.enabled {
            let mut controller = controllers::basic(processors::factory(
                selectors::simple::histogram(metrics_config.buckets.clone()),
                aggregation::stateless_temporality_selector(),
            ))
            .with_resource(Resource::new(
                metrics_config
                    .resources
                    .clone()
                    .into_iter()
                    .map(|(k, v)| KeyValue::new(k, v)),
            ))
            .build();

            // Check the last controller to see if the resources are the same, if they are we can use it as is.
            // Otherwise go with the new controller and store it so that it can be committed during telemetry activation.
            if let Some(last_controller) = CONTROLLER.lock().expect("lock poisoned").clone() {
                if controller.resource() == last_controller.resource() {
                    tracing::debug!("prometheus controller can be reused");
                    controller = last_controller
                } else {
                    tracing::debug!("prometheus controller cannot be reused");
                }
            }
            NEW_CONTROLLER
                .lock()
                .expect("lock poisoned")
                .replace(controller.clone());

            let exporter = opentelemetry_prometheus::exporter(controller).try_init()?;

            builder = builder.with_custom_endpoint(
                self.listen.clone(),
                Endpoint::from_router_service(
                    self.path.clone(),
                    PrometheusService {
                        registry: exporter.registry().clone(),
                    }
                    .boxed(),
                ),
            );
            builder = builder.with_meter_provider(FilterMeterProvider::public_metrics(
                exporter.meter_provider()?,
            ));
            builder = builder.with_exporter(exporter);
            tracing::info!(
                "Prometheus endpoint exposed at {}{}",
                self.listen,
                self.path
            );
        }
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
