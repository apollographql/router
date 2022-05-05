//! Telemetry customization.
use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config::{MetricsCommon, Trace};
use crate::plugins::telemetry::metrics::apollo::Sender;
use crate::plugins::telemetry::metrics::{
    AggregateMeterProvider, BasicMetrics, MetricsBuilder, MetricsConfigurator,
    MetricsExporterHandle,
};
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::subscriber::replace_layer;
use ::tracing::{info_span, Span};
use apollo_router_core::reexports::router_bridge::planner::UsageReporting;
use apollo_router_core::{
    http_compat, register_plugin, Context, ExecutionRequest, ExecutionResponse, Handler, Plugin,
    QueryPlannerRequest, QueryPlannerResponse, ResponseBody, RouterRequest, RouterResponse,
    ServiceBuilderExt as ServiceBuilderExt2, SubgraphRequest, SubgraphResponse, USAGE_REPORTING,
};
use apollo_spaceport::server::ReportSpaceport;
use bytes::Bytes;
use futures::FutureExt;
use http::{HeaderValue, StatusCode};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::propagation::{
    BaggagePropagator, TextMapCompositePropagator, TraceContextPropagator,
};
use opentelemetry::sdk::trace::Builder;
use opentelemetry::trace::{SpanKind, Tracer, TracerProvider};
use opentelemetry::{global, KeyValue};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::Instant;
use tower::steer::Steer;
use tower::util::BoxService;
use tower::{service_fn, BoxError, ServiceBuilder, ServiceExt};
use url::Url;

mod apollo;
pub mod config;
mod metrics;
mod otlp;
mod tracing;

pub static ROUTER_SPAN_NAME: &str = "router";
static CLIENT_NAME: &str = "apollo_telemetry::client_name";
static CLIENT_VERSION: &str = "apollo_telemetry::client_version";
pub(crate) static EXCLUDE: &str = "apollo_telemetry::studio::exclude";

pub struct Telemetry {
    config: config::Conf,
    tracer_provider: Option<opentelemetry::sdk::trace::TracerProvider>,
    // Do not remove _metrics_exporters. Metrics will not be exported if it is removed.
    // Typically the handles are a PushController but may be something else. Dropping the handle will
    // shutdown exporter.
    _metrics_exporters: Vec<MetricsExporterHandle>,
    meter_provider: AggregateMeterProvider,
    custom_endpoints: HashMap<String, Handler>,
    spaceport_shutdown: Option<futures::channel::oneshot::Sender<()>>,
    apollo_metrics_sender: metrics::apollo::Sender,
}

#[derive(Debug)]
struct ReportingError;

impl fmt::Display for ReportingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReportingError")
    }
}

impl std::error::Error for ReportingError {}

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

impl Drop for Telemetry {
    fn drop(&mut self) {
        if let Some(tracer_provider) = self.tracer_provider.take() {
            // Tracer providers must be flushed. This may happen as part of otel if the provider was set
            // as the global, but may also happen in the case of an failed config reload.
            // If the tracer prover is present then it was not handed over so we must flush it.
            // We must make the call to force_flush() from spawn_blocking() (or spawn a thread) to
            // ensure that the call to force_flush() is made from a separate thread.
            ::tracing::debug!("flushing telemetry");
            let jh = tokio::task::spawn_blocking(move || {
                opentelemetry::trace::TracerProvider::force_flush(&tracer_provider);
            });
            futures::executor::block_on(jh).expect("failed to flush tracer provider");
        }

        if let Some(sender) = self.spaceport_shutdown.take() {
            ::tracing::debug!("notifying spaceport to shut down");
            let _ = sender.send(());
        }
    }
}

#[async_trait::async_trait]
impl Plugin for Telemetry {
    type Config = config::Conf;

    fn activate(&mut self) {
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
        Self::replace_tracer_provider(tracer_provider);

        replace_layer(Box::new(telemetry))
            .expect("set_global_subscriber() was not called at startup, fatal");
        opentelemetry::global::set_error_handler(handle_error)
            .expect("otel error handler lock poisoned, fatal");
        global::set_text_map_propagator(Self::create_propagator(&self.config));
    }

    async fn new(mut config: Self::Config) -> Result<Self, BoxError> {
        // Apollo config is special because we enable tracing if some env variables are present.
        let apollo = config.apollo.get_or_insert_with(Default::default);
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
                ..
            } => {
                ::tracing::debug!("starting Spaceport");
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

        // Setup metrics
        // The act of setting up metrics will overwrite a global meter. However it is essential that
        // we use the aggregate meter provider that is created below. It enables us to support
        // sending metrics to multiple providers at once, of which hopefully Apollo Studio will
        // eventually be one.
        let mut builder = Self::create_metrics_exporters(&config)?;

        //// THIS IS IMPORTANT
        // Once the trace provider has been created this method MUST NOT FAIL
        // The trace provider will not be shut down if drop is not called and it will result in a hang.
        // Don't add anything fallible after the tracer provider has been created.
        let tracer_provider = Self::create_tracer_provider(&config)?;

        let plugin = Ok(Telemetry {
            spaceport_shutdown: shutdown_tx,
            tracer_provider: Some(tracer_provider),
            custom_endpoints: builder.custom_endpoints(),
            _metrics_exporters: builder.exporters(),
            meter_provider: builder.meter_provider(),
            apollo_metrics_sender: builder.apollo_metrics_provider(),
            config,
        });

        // We're safe now for shutdown.
        // Start spaceport
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

        plugin
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        let metrics_sender = self.apollo_metrics_sender.clone();
        let metrics = BasicMetrics::new(&self.meter_provider);
        let config = self.config.apollo.clone().unwrap_or_default();
        ServiceBuilder::new()
            .instrument(Self::router_service_span(config.clone()))
            .map_future_with_context(
                move |req: &RouterRequest| Self::populate_context(&config, req),
                move |ctx, fut| {
                    let metrics = metrics.clone();
                    let sender = metrics_sender.clone();
                    async move {
                        let result: Result<RouterResponse, BoxError> = fut.await;
                        Self::update_apollo_metrics(ctx, sender, &result);
                        Self::update_metrics(metrics, &result);
                        result
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        ServiceBuilder::new()
            .instrument(move |_| info_span!("query_planning", "otel.kind" = %SpanKind::Internal))
            .service(service)
            .boxed()
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        ServiceBuilder::new()
            .instrument(move |_| info_span!("execution", "otel.kind" = %SpanKind::Internal))
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        let metrics = BasicMetrics::new(&self.meter_provider);
        let subgraph_attribute = KeyValue::new("subgraph", name.to_string());
        let name = name.to_owned();
        ServiceBuilder::new()
            .instrument(move |_| info_span!("subgraph", name = name.as_str(), "otel.kind" = %SpanKind::Client))
            .service(service)
            .map_future(move |f| {
                let metrics = metrics.clone();
                let subgraph_attribute = subgraph_attribute.clone();
                // Using Instant because it is guaranteed to be monotonically increasing.
                let now = Instant::now();
                f.map(move |r| {
                    match &r {
                        Ok(response) => {
                            metrics.http_requests_total.add(
                                1,
                                &[
                                    KeyValue::new(
                                        "status",
                                        response.response.status().as_u16().to_string(),
                                    ),
                                    subgraph_attribute.clone(),
                                ],
                            );
                        }
                        Err(_) => {
                            metrics
                                .http_requests_error_total
                                .add(1, &[subgraph_attribute.clone()]);
                        }
                    }
                    metrics
                        .http_requests_duration
                        .record(now.elapsed().as_secs_f64(), &[subgraph_attribute.clone()]);
                    r
                })
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
                    .uri()
                    .path()
                    .trim_start_matches("/plugins/apollo.telemetry");
                if let Some(index) = paths.iter().position(|path| path == endpoint) {
                    index
                } else {
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
        // TODO Apollo tracing at some point in the future.
        // This is the shell of what was previously used to transmit metrics, but will in future be useful for sending traces.
        // builder = setup_tracing(builder, &config.apollo, trace_config)?;
        let tracer_provider = builder.build();
        Ok(tracer_provider)
    }

    fn create_metrics_exporters(config: &config::Conf) -> Result<MetricsBuilder, BoxError> {
        let metrics_config = config.metrics.clone().unwrap_or_default();
        let metrics_common_config = &metrics_config.common.unwrap_or_default();
        let mut builder = MetricsBuilder::default();
        builder = setup_metrics_exporter(builder, &config.apollo, metrics_common_config)?;
        builder =
            setup_metrics_exporter(builder, &metrics_config.prometheus, metrics_common_config)?;
        builder = setup_metrics_exporter(builder, &metrics_config.otlp, metrics_common_config)?;
        Ok(builder)
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

    fn replace_tracer_provider<T>(tracer_provider: T)
    where
        T: TracerProvider + Send + Sync + 'static,
        <T as TracerProvider>::Tracer: Send + Sync + 'static,
        <<T as opentelemetry::trace::TracerProvider>::Tracer as Tracer>::Span:
            Send + Sync + 'static,
    {
        let jh = tokio::task::spawn_blocking(|| {
            opentelemetry::global::force_flush_tracer_provider();
            opentelemetry::global::set_tracer_provider(tracer_provider);
        });
        futures::executor::block_on(jh).expect("failed to replace tracer provider");
    }

    fn router_service_span(config: apollo::Config) -> impl Fn(&RouterRequest) -> Span + Clone {
        let client_name_header = config.client_name_header;
        let client_version_header = config.client_version_header;

        move |request: &RouterRequest| {
            let http_request = &request.originating_request;
            let headers = http_request.headers();
            let query = http_request.body().query.clone().unwrap_or_default();
            let operation_name = http_request
                .body()
                .operation_name
                .clone()
                .unwrap_or_default();
            let client_name = headers
                .get(&client_name_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""));
            let client_version = headers
                .get(&client_version_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""));
            let span = info_span!(
                ROUTER_SPAN_NAME,
                query = query.as_str(),
                operation_name = operation_name.as_str(),
                client_name = client_name.to_str().unwrap_or_default(),
                client_version = client_version.to_str().unwrap_or_default(),
                "otel.kind" = %SpanKind::Internal
            );
            span
        }
    }

    fn update_apollo_metrics(
        context: Context,
        sender: Sender,
        _result: &Result<RouterResponse, BoxError>,
    ) {
        if let Some(usage_reporting) = context
            .get::<_, UsageReporting>(USAGE_REPORTING)
            .unwrap_or_default()
        {
            let exclude: Option<bool> = context.get(EXCLUDE).unwrap_or_default();
            let operation_count = operation_count(&usage_reporting.stats_report_key);

            //The basic metrics only has operation count
            let mut apollo_metrics = metrics::apollo::Metrics {
                operation_count,
                client_name: context
                    .get(CLIENT_NAME)
                    .unwrap_or_default()
                    .unwrap_or_default(),
                client_version: context
                    .get(CLIENT_NAME)
                    .unwrap_or_default()
                    .unwrap_or_default(),
                stats_report_key: usage_reporting.stats_report_key.to_string(),
                ..Default::default()
            };
            if !exclude.unwrap_or_default() {
                apollo_metrics.referenced_fields_by_type = usage_reporting
                    .referenced_fields_by_type
                    .into_iter()
                    .map(|(k, v)| (k, convert(v)))
                    .collect();
            };
            sender.send(apollo_metrics);
        };
    }

    fn update_metrics(metrics: BasicMetrics, result: &Result<RouterResponse, BoxError>) {
        // Using Instant because it is guaranteed to be monotonically increasing.
        let now = Instant::now();
        match &result {
            Ok(response) => {
                metrics.http_requests_total.add(
                    1,
                    &[KeyValue::new(
                        "status",
                        response.response.status().as_u16().to_string(),
                    )],
                );
            }
            Err(_) => {
                metrics.http_requests_error_total.add(1, &[]);
            }
        }
        metrics
            .http_requests_duration
            .record(now.elapsed().as_secs_f64(), &[]);
    }

    fn populate_context(config: &Config, req: &RouterRequest) -> Context {
        let context = req.context.clone();
        let http_request = &req.originating_request;
        let headers = http_request.headers();
        let client_name_header = &config.client_name_header;
        let client_version_header = &config.client_version_header;
        let _ = context.insert(
            CLIENT_NAME,
            headers
                .get(client_name_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""))
                .to_str()
                .unwrap_or_default()
                .to_string(),
        );
        let _ = context.insert(
            CLIENT_VERSION,
            headers
                .get(client_version_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""))
                .to_str()
                .unwrap_or_default()
                .to_string(),
        );
        context
    }
}

// Planner errors return stats report key that start with `## `
// while successful planning stats report key start with `# `
fn operation_count(stats_report_key: &str) -> u64 {
    if stats_report_key.starts_with("## ") {
        0
    } else {
        1
    }
}

fn convert(
    referenced_fields: apollo_router_core::reexports::router_bridge::planner::ReferencedFieldsForType,
) -> apollo_spaceport::ReferencedFieldsForType {
    apollo_spaceport::ReferencedFieldsForType {
        field_names: referenced_fields.field_names,
        is_interface: referenced_fields.is_interface,
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
            .await
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
            .await
            .unwrap();
    }
}
