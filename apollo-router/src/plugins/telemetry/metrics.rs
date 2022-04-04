use apollo_router_core::{
    http_compat, Handler, Plugin, ResponseBody, RouterRequest, RouterResponse, SubgraphRequest,
    SubgraphResponse,
};
use bytes::Bytes;
use futures::future::BoxFuture;
use http::{Method, StatusCode};
use opentelemetry::{
    global,
    metrics::{Counter, ValueRecorder},
    sdk::metrics::PushController,
    KeyValue,
};
use opentelemetry_prometheus::PrometheusExporter;
use prometheus::{Encoder, Registry, TextEncoder};
use reqwest::Url;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json_bytes::from_value;
use std::{
    task::{Context, Poll},
    time::SystemTime,
};
use tower::{service_fn, steer::Steer, util::BoxService, BoxError, Service, ServiceExt};

#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
use super::otlp::Metrics as OltpConfiguration;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MetricsConfiguration {
    pub exporter: MetricsExporter,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum MetricsExporter {
    Prometheus(PrometheusConfiguration),
    #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
    Otlp(Box<OltpConfiguration>),
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PrometheusConfiguration {
    endpoint: String,
}

#[derive(Debug)]
pub struct MetricsPlugin {
    exporter: Option<PrometheusExporter>,
    conf: MetricsConfiguration,
    metrics_controller: Option<PushController>,
    router_metrics: BasicMetrics,
    subgraph_metrics: BasicMetrics,
}

#[derive(Debug)]
pub struct BasicMetrics {
    http_requests_total: Counter<u64>,
    http_requests_error_total: Counter<u64>,
    http_requests_duration: ValueRecorder<f64>,
}

#[async_trait::async_trait]
impl Plugin for MetricsPlugin {
    type Config = MetricsConfiguration;

    fn new(config: Self::Config) -> Result<Self, BoxError> {
        let exporter = opentelemetry_prometheus::exporter().init();
        let meter = global::meter("apollo/router");

        if let MetricsExporter::Prometheus(prom_exporter_cfg) = &config.exporter {
            if Url::parse(&format!("http://test:8080{}", prom_exporter_cfg.endpoint)).is_err() {
                return Err(BoxError::from(
                    "cannot use your endpoint set for prometheus as a path in an URL, your path need to be absolute (starting with a '/')",
                ));
            }
        }

        Ok(Self {
            exporter: exporter.into(),
            conf: config,
            router_metrics: BasicMetrics {
                http_requests_total: meter
                    .u64_counter("http_requests_total")
                    .with_description("Total number of HTTP requests made.")
                    .init(),
                http_requests_error_total: meter
                    .u64_counter("http_requests_error_total")
                    .with_description("Total number of HTTP requests in error made.")
                    .init(),
                http_requests_duration: meter
                    .f64_value_recorder("http_request_duration_seconds")
                    .with_description("The HTTP request latencies in seconds.")
                    .init(),
            },
            subgraph_metrics: BasicMetrics {
                http_requests_total: meter
                    .u64_counter("http_requests_total_subgraph")
                    .with_description("Total number of HTTP requests made for a subgraph.")
                    .init(),
                http_requests_error_total: meter
                    .u64_counter("http_requests_error_total_subgraph")
                    .with_description("Total number of HTTP requests in error made for a subgraph.")
                    .init(),
                http_requests_duration: meter
                    .f64_value_recorder("http_request_duration_seconds_subgraph")
                    .with_description("The HTTP request latencies in seconds for a subgraph.")
                    .init(),
            },
            metrics_controller: None,
        })
    }

    async fn startup(&mut self) -> Result<(), BoxError> {
        match &self.conf.exporter {
            MetricsExporter::Prometheus(_) => {}
            #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
            MetricsExporter::Otlp(otlp_conf) => {
                self.metrics_controller = otlp_conf.exporter.metrics_exporter()?.into();
            }
        }

        Ok(())
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        const METRICS_REQUEST_TIME: &str = "METRICS_REQUEST_TIME";
        let http_counter = self.router_metrics.http_requests_total.clone();
        let http_request_duration = self.router_metrics.http_requests_duration.clone();
        let http_requests_error_total = self.router_metrics.http_requests_error_total.clone();

        service
            .map_request(|req: RouterRequest| {
                let request_start = SystemTime::now();
                req.context
                    .insert(METRICS_REQUEST_TIME, request_start)
                    .unwrap();

                req
            })
            .map_response(move |res| {
                let request_start: SystemTime = from_value(
                    res.context
                        .extensions
                        .get(METRICS_REQUEST_TIME)
                        .unwrap()
                        .clone(),
                )
                .unwrap();
                let kvs = &[
                    KeyValue::new("url", res.context.request.url().to_string()),
                    KeyValue::new("status", res.response.status().as_u16().to_string()),
                ];
                http_request_duration.record(
                    request_start.elapsed().map_or(0.0, |d| d.as_secs_f64()),
                    kvs,
                );
                http_counter.add(1, kvs);
                res
            })
            .map_err(move |err: BoxError| {
                http_requests_error_total.add(1, &[]);

                err
            })
            .boxed()
    }

    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        const METRICS_REQUEST_TIME: &str = "METRICS_REQUEST_TIME_SUBGRAPH";
        let subgraph_name = name.to_owned();
        let subgraph_name_cloned = name.to_owned();
        let subgraph_name_cloned_for_err = name.to_owned();
        let http_counter = self.subgraph_metrics.http_requests_total.clone();
        let http_request_duration = self.subgraph_metrics.http_requests_duration.clone();
        let http_requests_error_total = self.subgraph_metrics.http_requests_error_total.clone();

        service
            .map_request(move |req: SubgraphRequest| {
                let request_start = SystemTime::now();
                req.context
                    .insert(
                        format!("{}_{}", METRICS_REQUEST_TIME, subgraph_name),
                        request_start,
                    )
                    .unwrap();

                req
            })
            .map_response(move |res| {
                let request_start: SystemTime = from_value(
                    res.context
                        .extensions
                        .get(&format!(
                            "{}_{}",
                            METRICS_REQUEST_TIME, subgraph_name_cloned
                        ))
                        .unwrap()
                        .clone(),
                )
                .unwrap();
                let kvs = &[
                    KeyValue::new("subgraph", subgraph_name_cloned),
                    KeyValue::new("url", res.context.request.url().to_string()),
                    KeyValue::new("status", res.response.status().as_u16().to_string()),
                ];
                http_request_duration.record(
                    request_start.elapsed().map_or(0.0, |d| d.as_secs_f64()),
                    kvs,
                );
                http_counter.add(1, kvs);
                res
            })
            .map_err(move |err: BoxError| {
                http_requests_error_total.add(
                    1,
                    &[KeyValue::new("subgraph", subgraph_name_cloned_for_err)],
                );

                err
            })
            .boxed()
    }

    fn custom_endpoint(&self) -> Option<Handler> {
        let prometheus_endpoint = match &self.conf.exporter {
            MetricsExporter::Prometheus(prom) => Some(prom.endpoint.clone()),
            #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
            MetricsExporter::Otlp(_) => None,
        };

        match (prometheus_endpoint, &self.exporter) {
            (Some(endpoint), Some(exporter)) => {
                let registry = exporter.registry().clone();

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
            _ => None,
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
