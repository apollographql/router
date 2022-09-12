use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
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
use crate::services::transport;
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
            let exporter = opentelemetry_prometheus::exporter()
                .with_default_histogram_boundaries(vec![
                    0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0,
                ])
                .with_resource(Resource::new(
                    metrics_config
                        .resources
                        .clone()
                        .into_iter()
                        .map(|(k, v)| KeyValue::new(k, v)),
                ))
                .try_init()?;

            builder = builder.with_custom_endpoint(
                self.listen.clone(),
                Endpoint::new(
                    self.path.clone(),
                    PrometheusService {
                        registry: exporter.registry().clone(),
                    }
                    .boxed(),
                ),
            );
            builder = builder.with_meter_provider(exporter.provider()?);
            builder = builder.with_exporter(exporter);
        }
        Ok(builder)
    }
}

#[derive(Clone)]
pub(crate) struct PrometheusService {
    registry: Registry,
}

impl Service<transport::Request> for PrometheusService {
    type Response = transport::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, _req: transport::Request) -> Self::Future {
        let metric_families = self.registry.gather();
        Box::pin(async move {
            let encoder = TextEncoder::new();
            let mut result = Vec::new();
            encoder.encode(&metric_families, &mut result)?;
            http::Response::builder()
                .status(StatusCode::OK)
                .body(result.into())
                .map_err(|err| BoxError::from(err.to_string()))
        })
    }
}
