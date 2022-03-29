use apollo_router_core::{
    http_compat, Handler, Plugin, ResponseBody, RouterRequest, RouterResponse,
};
use http::StatusCode;
use opentelemetry::{global, metrics::Counter, KeyValue};
use opentelemetry_prometheus::PrometheusExporter;
use prometheus::{Encoder, TextEncoder};
use reqwest::Url;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower::{util::BoxService, BoxError, ServiceExt};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MetricsConfiguration {
    exporter: MetricsExporter,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum MetricsExporter {
    Prometheus(PrometheusConfiguration),
    // TODO, there are already todos in the oltp mod
    // OLTP(OltpConfiguration),
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PrometheusConfiguration {
    endpoint: String,
}

#[derive(Debug)]
pub struct MetricsPlugin {
    exporter: PrometheusExporter,
    conf: MetricsConfiguration,
    http_requests_total: Counter<u64>,
}

impl Plugin for MetricsPlugin {
    type Config = MetricsConfiguration;

    fn new(mut config: Self::Config) -> Result<Self, BoxError> {
        let exporter = opentelemetry_prometheus::exporter().init();
        let meter = global::meter("apollo/router");

        // TODO to delete when oltp is implemented
        #[allow(irrefutable_let_patterns)]
        if let MetricsExporter::Prometheus(prom_exporter_cfg) = &mut config.exporter {
            prom_exporter_cfg.endpoint = prom_exporter_cfg
                .endpoint
                .trim_start_matches('/')
                .to_string();

            if Url::parse(&format!("http://test/{}", prom_exporter_cfg.endpoint)).is_err() {
                return Err(BoxError::from(
                    "cannot use your endpoint set for prometheus as a path in an URL",
                ));
            }
        }

        Ok(Self {
            exporter,
            conf: config,
            http_requests_total: meter
                .u64_counter("http_requests_total")
                .with_description("Total number of HTTP requests made")
                .init(),
        })
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        let http_counter = self.http_requests_total.clone();

        service
            .map_request(move |req: RouterRequest| {
                http_counter.add(
                    1,
                    &[KeyValue::new("url", req.context.request.url().to_string())],
                );
                req
            })
            .boxed()
    }

    fn custom_endpoint(&self) -> Option<(String, Handler)> {
        let prometheus_endpoint = match &self.conf.exporter {
            MetricsExporter::Prometheus(prom) => Some(prom.endpoint.clone()),
            // MetricsExporter::OLTP(_) => None,
        };

        match prometheus_endpoint {
            Some(endpoint) => {
                let registry = self.exporter.registry().clone();

                let handler = move |_req| {
                    let encoder = TextEncoder::new();
                    let metric_families = registry.gather();
                    let mut result = Vec::new();
                    encoder.encode(&metric_families, &mut result).unwrap();

                    http_compat::Response {
                        inner: http::Response::builder()
                            .status(StatusCode::OK)
                            .body(ResponseBody::Text(
                                String::from_utf8_lossy(&result).into_owned(),
                            ))
                            .unwrap(),
                    }
                };

                Some((endpoint, Arc::new(handler)))
            }
            None => None,
        }
    }
}
