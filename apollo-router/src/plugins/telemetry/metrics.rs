use apollo_router_core::{
    http_compat, Handler, Plugin, ResponseBody, RouterRequest, RouterResponse,
};
use bytes::Bytes;
use futures::future::BoxFuture;
use http::{Method, StatusCode};
use opentelemetry::{global, metrics::Counter, KeyValue};
use opentelemetry_prometheus::PrometheusExporter;
use prometheus::{Encoder, Registry, TextEncoder};
use reqwest::Url;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::task::{Context, Poll};
use tower::{service_fn, steer::Steer, util::BoxService, BoxError, Service, ServiceExt};

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

    fn new(config: Self::Config) -> Result<Self, BoxError> {
        let exporter = opentelemetry_prometheus::exporter().init();
        let meter = global::meter("apollo/router");

        // TODO to delete when oltp is implemented
        #[allow(irrefutable_let_patterns)]
        if let MetricsExporter::Prometheus(prom_exporter_cfg) = &config.exporter {
            if Url::parse(&format!("http://test:8080{}", prom_exporter_cfg.endpoint)).is_err() {
                return Err(BoxError::from(
                    "cannot use your endpoint set for prometheus as a path in an URL, your path need to be absolute (starting with a '/'",
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

    fn custom_endpoint(&self) -> Option<Handler> {
        let prometheus_endpoint = match &self.conf.exporter {
            MetricsExporter::Prometheus(prom) => Some(prom.endpoint.clone()),
            // MetricsExporter::OLTP(_) => None,
        };

        match prometheus_endpoint {
            Some(endpoint) => {
                let registry = self.exporter.registry().clone();

                let not_found_handler = service_fn(|_req: http_compat::Request<Bytes>| async {
                    Ok::<_, BoxError>(http_compat::Response {
                        inner: http::Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(ResponseBody::Text(String::new()))
                            .unwrap(),
                    })
                })
                .boxed();
                let metrics_handler = PrometheusService { registry }.boxed();

                let svc = Steer::new(
                    // All services we route between
                    vec![metrics_handler, not_found_handler],
                    // How we pick which service to send the request to
                    move |req: &http_compat::Request<Bytes>, _services: &[_]| {
                        if req.method() == Method::GET
                            && req
                                .url()
                                .path()
                                .trim_start_matches("/plugins/apollo.telemetry")
                                == endpoint
                        {
                            0 // Index of `metrics handler`
                        } else {
                            1 // Index of `not_found`
                        }
                    },
                );

                Some(svc.boxed().into())
            }
            None => None,
        }
    }
}

#[derive(Clone)]
pub struct PrometheusService {
    registry: Registry,
}

impl Service<http_compat::Request<Bytes>> for PrometheusService {
    type Response = http_compat::Response<ResponseBody>;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, _req: http_compat::Request<Bytes>) -> Self::Future {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut result = Vec::new();
        encoder.encode(&metric_families, &mut result).unwrap();

        Box::pin(async move {
            Ok(http_compat::Response {
                inner: http::Response::builder()
                    .status(StatusCode::OK)
                    .body(ResponseBody::Text(
                        String::from_utf8_lossy(&result).into_owned(),
                    ))
                    .map_err(|err| BoxError::from(err.to_string()))?,
            })
        })
    }
}
