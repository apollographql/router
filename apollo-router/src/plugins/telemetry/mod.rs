//! Telemetry customization.
use crate::plugins::telemetry::config::{MetricsCommon, Trace};
use crate::plugins::telemetry::tracing::apollo;
use crate::subscriber::replace_layer;
use apollo_router_core::{
    http_compat, register_plugin, Handler, Plugin, ResponseBody, RouterRequest, RouterResponse,
    SubgraphRequest, SubgraphResponse,
};
use apollo_spaceport::server::ReportSpaceport;
use bytes::Bytes;
use futures::FutureExt;
use http::StatusCode;
use opentelemetry::metrics::{Counter, ValueRecorder};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::propagation::{
    BaggagePropagator, TextMapCompositePropagator, TraceContextPropagator,
};
use opentelemetry::sdk::trace::Builder;
use opentelemetry::trace::TracerProvider;
use opentelemetry::{global, KeyValue};
use std::any::Any;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::SystemTime;
use tower::steer::Steer;
use tower::util::BoxService;
use tower::{service_fn, BoxError, ServiceExt};
use url::Url;

mod config;
mod metrics;
mod otlp;
mod tracing;

pub struct Telemetry {
    config: config::Conf,
    tracer_provider: Option<opentelemetry::sdk::trace::TracerProvider>,

    // Do not remove _metrics_exporters. Metrics will not be exported if it is removed.
    // Typically the handles are a PushController but may be something else. Dropping the handle will
    // shutdown exporter.
    _metrics_exporters: Vec<MetricsExporterHandle>,
    custom_endpoints: HashMap<String, Handler>,

    spaceport_shutdown: Option<futures::channel::oneshot::Sender<()>>,
    pub router_metrics: BasicMetrics,
    pub subgraph_metrics: BasicMetrics,
}

#[derive(Debug)]
struct ReportingError;

impl fmt::Display for ReportingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReportingError")
    }
}

impl std::error::Error for ReportingError {}

trait TracingConfigurator {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError>;
}

#[derive(Debug)]
pub struct BasicMetrics {
    http_requests_total: Counter<u64>,
    http_requests_error_total: Counter<u64>,
    http_requests_duration: ValueRecorder<f64>,
}

type MetricsExporterHandle = Box<dyn Any + Send + Sync + 'static>;
type CustomEndpoint =
    BoxService<http_compat::Request<Bytes>, http_compat::Response<ResponseBody>, BoxError>;

#[derive(Default)]
struct MetricsBuilder {
    exporters: Vec<MetricsExporterHandle>,
    custom_endpoints: HashMap<String, Handler>,
}

impl MetricsBuilder {
    fn with_exporter<T: Send + Sync + 'static>(mut self, handle: T) -> Self {
        self.exporters.push(Box::new(handle));
        self
    }
    fn with_custom_endpoint(mut self, path: &str, endpoint: CustomEndpoint) -> Self {
        self.custom_endpoints
            .insert(path.to_string(), Handler::new(endpoint));
        self
    }
}

trait MetricsConfigurator {
    fn apply(
        &self,
        builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError>;
}

trait GenericWith<T>
where
    Self: Sized,
{
    fn with<B>(self, option: &Option<B>, apply: fn(Self, &B) -> Self) -> Self {
        if let Some(option) = option {
            return apply(self, option);
        }
        self
    }
    fn try_with<B>(
        self,
        option: &Option<B>,
        apply: fn(Self, &B) -> Result<Self, BoxError>,
    ) -> Result<Self, BoxError> {
        if let Some(option) = option {
            return apply(self, option);
        }
        Ok(self)
    }
}

impl<T> GenericWith<T> for T where Self: Sized {}

fn setup_tracing<T: TracingConfigurator>(
    mut builder: Builder,
    configurator: &Option<T>,
    tracing_config: &Trace,
) -> Result<Builder, BoxError> {
    if let Some(config) = configurator {
        builder = config.apply(builder, tracing_config)?;
    }
    Ok(builder)
}

fn setup_metrics_exporter<T: MetricsConfigurator>(
    mut builder: MetricsBuilder,
    configurator: &Option<T>,
    metrics_common: &MetricsCommon,
) -> Result<MetricsBuilder, BoxError> {
    if let Some(config) = configurator {
        builder = config.apply(builder, metrics_common)?;
    }
    Ok(builder)
}

fn apollo_key() -> Option<String> {
    std::env::var("APOLLO_KEY").ok()
}

fn apollo_graph_reference() -> Option<String> {
    std::env::var("APOLLO_GRAPH_REF").ok()
}

#[async_trait::async_trait]
impl Plugin for Telemetry {
    type Config = config::Conf;

    async fn startup(&mut self) -> Result<(), BoxError> {
        // Apollo config is special because we enable tracing if some env variables are present.
        let apollo = self.config.apollo.get_or_insert_with(Default::default);
        if apollo.apollo_key.is_none() {
            apollo.apollo_key = apollo_key()
        }
        if apollo.apollo_graph_ref.is_none() {
            apollo.apollo_graph_ref = apollo_graph_reference()
        }

        // If we have key and graph ref but no endpoint we start embedded spaceport
        let (spaceport, shutdown_tx) = match apollo {
            apollo::Config {
                apollo_key: Some(_),
                apollo_graph_ref: Some(_),
                endpoint: None,
            } => {
                let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel();
                let report_spaceport = ReportSpaceport::new(
                    "127.0.0.1:0".parse()?,
                    Some(Box::pin(shutdown_rx.map(|_| ()))),
                )
                .await?;
                // Now that the port is known update the config
                apollo.endpoint = Some(Url::parse(&format!(
                    "https://{}",
                    report_spaceport.address()
                ))?);
                (Some(report_spaceport), Some(shutdown_tx))
            }
            _ => (None, None),
        };

        //If store the shutdown handle.
        self.spaceport_shutdown = shutdown_tx;

        // Now that spaceport is set up it is possible to set up the tracer providers.
        self.tracer_provider = Some(Self::create_tracer_provider(&self.config)?);

        // Setup metrics
        let (exporters, custom_endpoints) = Self::create_metrics_exporters(&self.config)?;
        self._metrics_exporters = exporters;
        self.custom_endpoints = custom_endpoints;

        // Finally actually start spaceport
        if let Some(spaceport) = spaceport {
            tokio::spawn(async move {
                if let Err(e) = spaceport.serve().await {
                    match e.source() {
                        Some(source) => {
                            ::tracing::warn!("spaceport did not terminate normally: {}", source);
                        }
                        None => {
                            ::tracing::warn!("spaceport did not terminate normally: {}", e);
                        }
                    }
                };
            });
        }

        Ok(())
    }

    fn ready(&mut self) {
        // The active service is about to be swapped in.
        // The rest of this code in this method is expected to succeed.
        // The issue is that Otel uses globals for a bunch of stuff.
        // If we move to a completely tower based architecture then we could make this nicer.
        let tracer_provider = self
            .tracer_provider
            .take()
            .expect("trace_provider will have been set in startup, qed");

        let tracer = tracer_provider.versioned_tracer(
            "apollo-router",
            Some(env!("CARGO_PKG_VERSION")),
            None,
        );

        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        Self::flush_tracer();
        replace_layer(Box::new(telemetry))
            .expect("set_global_subscriber() was not called at startup, fatal");
        opentelemetry::global::set_error_handler(handle_error)
            .expect("otel error handler lock poisoned, fatal");
        global::set_text_map_propagator(Self::create_propagator(&self.config));
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        Self::flush_tracer();
        if let Some(sender) = self.spaceport_shutdown.take() {
            let _ = sender.send(());
        }
        Ok(())
    }

    fn new(config: Self::Config) -> Result<Self, BoxError> {
        let meter = global::meter("apollo/router");

        Ok(Telemetry {
            spaceport_shutdown: None,
            tracer_provider: None,
            custom_endpoints: Default::default(),
            _metrics_exporters: Default::default(),
            config,
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
        })
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
                let request_start: SystemTime = res
                    .context
                    .get::<_, SystemTime>(METRICS_REQUEST_TIME)
                    .expect("timestamp will be correct type")
                    .expect("timestamp got inserted on request. qed");

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
        let extension_metric_name = format!("{}_{}", METRICS_REQUEST_TIME, subgraph_name);
        let extension_metric_name_cloned = extension_metric_name.clone();

        service
            .map_request(move |req: SubgraphRequest| {
                let request_start = SystemTime::now();
                req.context
                    .insert(extension_metric_name.clone(), request_start)
                    .unwrap();

                req
            })
            .map_response(move |res| {
                let request_start: SystemTime = res
                    .context
                    .get::<_, SystemTime>(&extension_metric_name_cloned)
                    .expect("timestamp will be correct type")
                    .expect("timestamp got inserted on request. qed");

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
        let (paths, mut endpoints): (Vec<_>, Vec<_>) =
            self.custom_endpoints.clone().into_iter().unzip();
        endpoints.push(Self::not_found_endpoint());
        let not_found_index = endpoints.len() - 1;

        let svc = Steer::new(
            // All services we route between
            endpoints,
            // How we pick which service to send the request to
            move |req: &http_compat::Request<Bytes>, _services: &[_]| {
                let endpoint = req
                    .url()
                    .path()
                    .trim_start_matches("/plugins/apollo.telemetry");
                if let Some(index) = paths.iter().position(|path| path == endpoint) {
                    ::tracing::info!("Returning  {}", index);
                    index
                } else {
                    ::tracing::info!("Returning error");
                    not_found_index
                }
            },
        )
        .boxed();

        Some(Handler::new(svc))
    }
}

impl Telemetry {
    fn create_propagator(config: &config::Conf) -> TextMapCompositePropagator {
        let propagation = config
            .clone()
            .tracing
            .and_then(|c| c.propagation)
            .unwrap_or_default();

        let tracing = config.clone().tracing.unwrap_or_default();

        let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
        if propagation.baggage.unwrap_or_default() {
            propagators.push(Box::new(BaggagePropagator::default()));
        }
        if propagation.trace_context.unwrap_or_default() || tracing.otlp.is_some() {
            propagators.push(Box::new(TraceContextPropagator::default()));
        }
        if propagation.zipkin.unwrap_or_default() || tracing.zipkin.is_some() {
            propagators.push(Box::new(opentelemetry_zipkin::Propagator::default()));
        }
        if propagation.jaeger.unwrap_or_default() || tracing.jaeger.is_some() {
            propagators.push(Box::new(opentelemetry_jaeger::Propagator::default()));
        }
        if propagation.datadog.unwrap_or_default() || tracing.datadog.is_some() {
            propagators.push(Box::new(opentelemetry_datadog::DatadogPropagator::default()));
        }

        TextMapCompositePropagator::new(propagators)
    }

    fn create_tracer_provider(
        config: &config::Conf,
    ) -> Result<opentelemetry::sdk::trace::TracerProvider, BoxError> {
        let tracing_config = config.tracing.clone().unwrap_or_default();
        let trace_config = &tracing_config.trace_config.unwrap_or_default();
        let mut builder =
            opentelemetry::sdk::trace::TracerProvider::builder().with_config(trace_config.into());

        builder = setup_tracing(builder, &tracing_config.jaeger, trace_config)?;
        builder = setup_tracing(builder, &tracing_config.zipkin, trace_config)?;
        builder = setup_tracing(builder, &tracing_config.datadog, trace_config)?;
        builder = setup_tracing(builder, &tracing_config.otlp, trace_config)?;
        builder = setup_tracing(builder, &config.apollo, trace_config)?;
        let tracer_provider = builder.build();
        Ok(tracer_provider)
    }

    fn create_metrics_exporters(
        config: &config::Conf,
    ) -> Result<(Vec<MetricsExporterHandle>, HashMap<String, Handler>), BoxError> {
        let metrics_config = config.metrics.clone().unwrap_or_default();
        let metrics_common_config = &metrics_config.common.unwrap_or_default();
        let mut builder = MetricsBuilder::default();
        builder =
            setup_metrics_exporter(builder, &metrics_config.prometheus, metrics_common_config)?;
        builder = setup_metrics_exporter(builder, &metrics_config.otlp, metrics_common_config)?;
        Ok((builder.exporters, builder.custom_endpoints))
    }

    fn flush_tracer() {
        // This code will hang unless we execute from a separate
        // thread.  See:
        // https://github.com/apollographql/router/issues/331
        // https://github.com/open-telemetry/opentelemetry-rust/issues/536
        // for more details and description.
        let jh = tokio::task::spawn_blocking(|| {
            opentelemetry::global::force_flush_tracer_provider();
        });
        futures::executor::block_on(jh).expect("could not flush previous tracer");
    }

    fn not_found_endpoint() -> Handler {
        Handler::new(
            service_fn(|_req: http_compat::Request<Bytes>| async {
                Ok::<_, BoxError>(http_compat::Response {
                    inner: http::Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(ResponseBody::Text("Not found".to_string()))
                        .unwrap(),
                })
            })
            .boxed(),
        )
    }
}

fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    match err.into() {
        opentelemetry::global::Error::Trace(err) => {
            ::tracing::error!("OpenTelemetry trace error occurred: {}", err)
        }
        opentelemetry::global::Error::Other(err_msg) => {
            ::tracing::error!("OpenTelemetry error occurred: {}", err_msg)
        }
        other => {
            ::tracing::error!("OpenTelemetry error occurred: {:?}", other)
        }
    }
}

register_plugin!("apollo", "telemetry", Telemetry);

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({ "tracing": null }))
            .unwrap();
    }

    #[tokio::test]
    async fn attribute_serialization() {
        apollo_router_core::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({
                "tracing": {

                    "trace_config": {
                        "service_name": "router",
                        "attributes": {
                            "str": "a",
                            "int": 1,
                            "float": 1.0,
                            "bool": true,
                            "str_arr": ["a", "b"],
                            "int_arr": [1, 2],
                            "float_arr": [1.0, 2.0],
                            "bool_arr": [true, false]
                            }
                        }
                    }
            }))
            .unwrap();
    }
}
