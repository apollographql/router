use apollo_router_core::{Plugin, ResponseBody, RouterRequest, RouterResponse};
use http::StatusCode;
use opentelemetry::{global, metrics::Counter, KeyValue};
use opentelemetry_prometheus::PrometheusExporter;
use prometheus::{Encoder, TextEncoder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::{util::BoxService, BoxError, ServiceExt};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MetricsConfiguration {
    exporter: MetricsExporter,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum MetricsExporter {
    Prometheus(PrometheusConfiguration),
    OLTP(OltpConfiguration),
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PrometheusConfiguration {
    endpoint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct OltpConfiguration {}

#[derive(Debug)]
pub struct MetricsPlugin {
    exporter: PrometheusExporter,
    conf: MetricsConfiguration,
    http_counter: Counter<u64>,
}

impl Plugin for MetricsPlugin {
    type Config = MetricsConfiguration;

    fn new(config: Self::Config) -> Result<Self, BoxError> {
        let exporter = opentelemetry_prometheus::exporter().init();
        let meter = global::meter("apollo/router");

        Ok(Self {
            exporter,
            conf: config,
            http_counter: meter
                .u64_counter("http_requests_total")
                .with_description("Total number of HTTP requests made")
                .init(),
        })
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        println!("heeere");
        let http_counter = self.http_counter.clone();
        let registry = self.exporter.registry().clone();
        let prometheus_endpoint = match &self.conf.exporter {
            MetricsExporter::Prometheus(prom) => Some(format!("/graphql{}", prom.endpoint.clone())),
            MetricsExporter::OLTP(_) => None,
        };

        service
            .map_request(move |req: RouterRequest| {
                http_counter.add(
                    1,
                    &[KeyValue::new("url", req.context.request.url().to_string())],
                );
                req
            })
            .map_response(move |mut response: RouterResponse| {
                if let Some(prometheus_endpoint) = prometheus_endpoint {
                    if response.context.request.url().path() == prometheus_endpoint {
                        let encoder = TextEncoder::new();
                        let metric_families = registry.gather();
                        let mut result = Vec::new();
                        encoder.encode(&metric_families, &mut result).unwrap();
                        *response.response.body_mut() =
                            ResponseBody::RawString(String::from_utf8_lossy(&result).into_owned());
                        *response.response.status_mut() = StatusCode::OK;
                    }
                }

                response
            })
            .boxed()
    }
}
