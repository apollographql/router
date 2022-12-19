use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
use opentelemetry::sdk::export::metrics::aggregation;
use opentelemetry::sdk::metrics::controllers;
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
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::router_factory::Endpoint;
use crate::services::router;
use crate::ListenAddr;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) enabled: bool,
    #[serde(default = "prometheus_default_listen_addr")]
    pub(crate) listen: ListenAddr,
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

impl MetricsConfigurator for Config {
    fn apply(
        &self,
        mut builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        if self.enabled {
            tracing::info!(
                "prometheus endpoint exposed at {}{}",
                self.listen,
                self.path
            );
            let controller = controllers::basic(
                processors::factory(
                    selectors::simple::histogram([
                        0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0,
                    ]),
                    aggregation::stateless_temporality_selector(),
                )
                .with_memory(true),
            )
            .with_resource(Resource::new(
                metrics_config
                    .resources
                    .clone()
                    .into_iter()
                    .map(|(k, v)| KeyValue::new(k, v)),
            ))
            .build();
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
            builder = builder.with_meter_provider(exporter.meter_provider()?);
            builder = builder.with_exporter(exporter);
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
            Ok(router::Response {
                response: http::Response::builder()
                    .status(StatusCode::OK)
                    .body::<hyper::Body>(result.into())
                    .map_err(BoxError::from)?,
                context: req.context,
            })
        })
    }
}
