//! Telemetry plugin.
// This entire file is license key functionality
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use ::tracing::info_span;
use ::tracing::Span;
use apollo_spaceport::server::ReportSpaceport;
use apollo_spaceport::StatsContext;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::StreamExt;
use http::HeaderValue;
use http::StatusCode;
use metrics::apollo::Sender;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::propagation::BaggagePropagator;
use opentelemetry::sdk::propagation::TextMapCompositePropagator;
use opentelemetry::sdk::propagation::TraceContextPropagator;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::Tracer;
use opentelemetry::trace::TracerProvider;
use opentelemetry::KeyValue;
use router_bridge::planner::UsageReporting;
use tower::service_fn;
use tower::steer::Steer;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

use self::config::Conf;
use self::metrics::AttributesForwardConf;
use self::metrics::MetricsAttributesConf;
use crate::http_ext;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Handler;
use crate::plugin::Plugin;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::metrics::apollo::studio::SingleContextualizedStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleQueryLatencyStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleReport;
use crate::plugins::telemetry::metrics::apollo::studio::SingleTracesAndStats;
use crate::plugins::telemetry::metrics::AggregateMeterProvider;
use crate::plugins::telemetry::metrics::BasicMetrics;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::metrics::MetricsExporterHandle;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::query_planner::USAGE_REPORTING;
use crate::register_plugin;
use crate::subscriber::replace_layer;
use crate::Context;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

pub mod apollo;
pub mod config;
mod metrics;
mod otlp;
mod tracing;

pub static ROUTER_SPAN_NAME: &str = "router";
static CLIENT_NAME: &str = "apollo_telemetry::client_name";
static CLIENT_VERSION: &str = "apollo_telemetry::client_version";
const ATTRIBUTES: &str = "apollo_telemetry::metrics_attributes";
const SUBGRAPH_ATTRIBUTES: &str = "apollo_telemetry::subgraph_metrics_attributes";
pub(crate) static STUDIO_EXCLUDE: &str = "apollo_telemetry::studio::exclude";
const SERVICE_NAME_RESOURCE: &str = "service.name";
const DEFAULT_SERVICE_NAME: &str = "apollo-router";

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

impl Drop for Telemetry {
    fn drop(&mut self) {
        if let Some(tracer_provider) = self.tracer_provider.take() {
            // Tracer providers must be flushed. This may happen as part of otel if the provider was set
            // as the global, but may also happen in the case of an failed config reload.
            // If the tracer prover is present then it was not handed over so we must flush it.
            // We must make the call to force_flush() from a separate thread to prevent hangs:
            // see https://github.com/open-telemetry/opentelemetry-rust/issues/536.
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
        let apollo = config
            .apollo
            .as_mut()
            .expect("telemetry apollo config must be present");

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
        //
        // let metrics_response_queries = Self::create_metrics_queries(&config)?;

        let plugin = Ok(Telemetry {
            spaceport_shutdown: shutdown_tx,
            tracer_provider: Some(tracer_provider),
            custom_endpoints: builder.custom_endpoints(),
            _metrics_exporters: builder.exporters(),
            meter_provider: builder.meter_provider(),
            apollo_metrics_sender: builder.apollo_metrics_provider(),
            config,
            // metrics_response_queries,
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
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        let metrics_sender = self.apollo_metrics_sender.clone();
        let metrics = BasicMetrics::new(&self.meter_provider);
        let config = Arc::new(self.config.clone());
        let config_map_res = config.clone();
        ServiceBuilder::new()
            .instrument(Self::router_service_span(
                config.apollo.clone().unwrap_or_default(),
            ))
            .map_future_with_context(
                move |req: &RouterRequest| {
                    Self::populate_context(config.clone(), req);
                    req.context.clone()
                },
                move |ctx: Context, fut| {
                    let config = config_map_res.clone();
                    let metrics = metrics.clone();
                    let sender = metrics_sender.clone();
                    let start = Instant::now();
                    async move {
                        let mut result: Result<RouterResponse, BoxError> = fut.await;
                        result = Self::update_metrics(
                            config.clone(),
                            ctx.clone(),
                            metrics,
                            result,
                            start.elapsed(),
                        )
                        .await;
                        match result {
                            Err(e) => {
                                if !matches!(sender, Sender::Noop) {
                                    Self::update_apollo_metrics(
                                        &ctx,
                                        sender,
                                        true,
                                        start.elapsed(),
                                    );
                                }

                                Err(e)
                            }
                            Ok(router_response) => {
                                let is_not_success =
                                    !router_response.response.status().is_success();
                                Ok(router_response.map(move |response_stream| {
                                    let sender = sender.clone();
                                    let ctx = ctx.clone();

                                    response_stream
                                        .map(move |response| {
                                            let response_has_errors = !response.errors.is_empty();

                                            if !matches!(sender, Sender::Noop) {
                                                Self::update_apollo_metrics(
                                                    &ctx,
                                                    sender.clone(),
                                                    is_not_success || response_has_errors,
                                                    start.elapsed(),
                                                );
                                            }
                                            response
                                        })
                                        .boxed()
                                }))
                            }
                        }
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn query_planning_service(
        &self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        ServiceBuilder::new()
            .instrument(move |_| info_span!("query_planning", "otel.kind" = %SpanKind::Internal))
            .service(service)
            .boxed()
    }

    fn execution_service(
        &self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        ServiceBuilder::new()
            .instrument(move |_| info_span!("execution", "otel.kind" = %SpanKind::Internal))
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        let metrics = BasicMetrics::new(&self.meter_provider);
        let subgraph_attribute = KeyValue::new("subgraph", name.to_string());
        let name = name.to_owned();
        let subgraph_metrics = Arc::new(
            self.config
                .metrics
                .as_ref()
                .and_then(|m| m.common.as_ref())
                .and_then(|c| c.attributes.as_ref())
                .and_then(|c| c.subgraph.as_ref())
                .map(|subgraph_cfg| {
                    macro_rules! extend_config {
                        ($forward_kind: ident) => {{
                            let mut cfg = subgraph_cfg
                                .all
                                .as_ref()
                                .and_then(|a| a.$forward_kind.clone())
                                .unwrap_or_default();
                            if let Some(subgraphs) = &subgraph_cfg.subgraphs {
                                cfg.extend(
                                    subgraphs
                                        .get(&name)
                                        .and_then(|s| s.$forward_kind.clone())
                                        .unwrap_or_default(),
                                );
                            }

                            cfg
                        }};
                    }
                    macro_rules! merge_config {
                        ($forward_kind: ident) => {{
                            let mut cfg = subgraph_cfg
                                .all
                                .as_ref()
                                .and_then(|a| a.$forward_kind.clone())
                                .unwrap_or_default();
                            if let Some(subgraphs) = &subgraph_cfg.subgraphs {
                                cfg.merge(
                                    subgraphs
                                        .get(&name)
                                        .and_then(|s| s.$forward_kind.clone())
                                        .unwrap_or_default(),
                                );
                            }

                            cfg
                        }};
                    }
                    let insert = extend_config!(insert);
                    let context = extend_config!(context);
                    let request = merge_config!(request);
                    let response = merge_config!(response);

                    AttributesForwardConf {
                        insert: (!insert.is_empty()).then(|| insert),
                        request: (request.header.is_some() || request.body.is_some())
                            .then(|| request),
                        response: (response.header.is_some() || response.body.is_some())
                            .then(|| response),
                        context: (!context.is_empty()).then(|| context),
                    }
                }),
        );
        let subgraph_metrics_conf = subgraph_metrics.clone();
        ServiceBuilder::new()
            .instrument(move |_| {
                info_span!("subgraph",
                    name = name.as_str(),
                    "otel.kind" = %SpanKind::Internal,
                )
            })
            .map_future_with_context(
                move |sub_request: &SubgraphRequest| {
                    let subgraph_metrics_conf = subgraph_metrics_conf.clone();
                    let mut attributes = HashMap::new();
                    if let Some(subgraph_attributes_conf) = &*subgraph_metrics_conf {
                        attributes.extend(subgraph_attributes_conf.get_attributes_from_request(
                            sub_request.subgraph_request.headers(),
                            sub_request.subgraph_request.body(),
                        ));
                        attributes.extend(
                            subgraph_attributes_conf
                                .get_attributes_from_context(&sub_request.context),
                        );
                    }
                    sub_request
                        .context
                        .insert(SUBGRAPH_ATTRIBUTES, attributes)
                        .unwrap();

                    sub_request.context.clone()
                },
                move |context: Context,
                      f: BoxFuture<'static, Result<SubgraphResponse, BoxError>>| {
                    let metrics = metrics.clone();
                    let subgraph_attribute = subgraph_attribute.clone();
                    let subgraph_metrics = subgraph_metrics.clone();
                    // Using Instant because it is guaranteed to be monotonically increasing.
                    let now = Instant::now();
                    f.map(move |r: Result<SubgraphResponse, BoxError>| {
                        let subgraph_metrics_conf = subgraph_metrics.clone();
                        let mut metric_attrs = context
                            .get::<_, HashMap<String, String>>(SUBGRAPH_ATTRIBUTES)
                            .ok()
                            .flatten()
                            .map(|attrs| {
                                attrs
                                    .into_iter()
                                    .map(|(attr_name, attr_value)| {
                                        KeyValue::new(attr_name, attr_value)
                                    })
                                    .collect::<Vec<KeyValue>>()
                            })
                            .unwrap_or_default();
                        metric_attrs.push(subgraph_attribute.clone());
                        // Fill attributes from context
                        if let Some(subgraph_attributes_conf) = &*subgraph_metrics_conf {
                            metric_attrs.extend(
                                subgraph_attributes_conf
                                    .get_attributes_from_context(&context)
                                    .into_iter()
                                    .map(|(k, v)| KeyValue::new(k, v)),
                            );
                        }

                        match &r {
                            Ok(response) => {
                                metric_attrs.push(KeyValue::new(
                                    "status",
                                    response.response.status().as_u16().to_string(),
                                ));

                                // Fill attributes from response
                                if let Some(subgraph_attributes_conf) = &*subgraph_metrics_conf {
                                    metric_attrs.extend(
                                        subgraph_attributes_conf
                                            .get_attributes_from_response(
                                                response.response.headers(),
                                                response.response.body(),
                                            )
                                            .into_iter()
                                            .map(|(k, v)| KeyValue::new(k, v)),
                                    );
                                }

                                metrics.http_requests_total.add(1, &metric_attrs);
                            }
                            Err(_) => {
                                metrics.http_requests_error_total.add(1, &metric_attrs);
                            }
                        }
                        metrics
                            .http_requests_duration
                            .record(now.elapsed().as_secs_f64(), &metric_attrs);
                        r
                    })
                },
            )
            .service(service)
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
            move |req: &http_ext::Request<Bytes>, _services: &[_]| {
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
        let metrics_common_config = &mut metrics_config.common.unwrap_or_default();
        // Set default service name for metrics
        if metrics_common_config
            .resources
            .get(SERVICE_NAME_RESOURCE)
            .is_none()
        {
            metrics_common_config.resources.insert(
                String::from(SERVICE_NAME_RESOURCE),
                String::from(DEFAULT_SERVICE_NAME),
            );
        }

        let mut builder = MetricsBuilder::default();
        builder = setup_metrics_exporter(builder, &config.apollo, metrics_common_config)?;
        builder =
            setup_metrics_exporter(builder, &metrics_config.prometheus, metrics_common_config)?;
        builder = setup_metrics_exporter(builder, &metrics_config.otlp, metrics_common_config)?;
        Ok(builder)
    }

    fn not_found_endpoint() -> Handler {
        Handler::new(
            service_fn(|_req: http_ext::Request<Bytes>| async {
                Ok::<_, BoxError>(http_ext::Response {
                    inner: http::Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body("Not found".into())
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
        context: &Context,
        sender: Sender,
        has_errors: bool,
        duration: Duration,
    ) {
        let metrics = if let Some(usage_reporting) = context
            .get::<_, UsageReporting>(USAGE_REPORTING)
            .unwrap_or_default()
        {
            let operation_count = operation_count(&usage_reporting.stats_report_key);
            let persisted_query_hit = context
                .get::<_, bool>("persisted_query_hit")
                .unwrap_or_default();

            if context
                .get(STUDIO_EXCLUDE)
                .map_or(false, |x| x.unwrap_or_default())
            {
                // The request was excluded don't report the details, but do report the operation count
                SingleReport {
                    operation_count,
                    ..Default::default()
                }
            } else {
                metrics::apollo::studio::SingleReport {
                    operation_count,
                    traces_and_stats: HashMap::from([(
                        usage_reporting.stats_report_key.to_string(),
                        SingleTracesAndStats {
                            stats_with_context: SingleContextualizedStats {
                                context: StatsContext {
                                    client_name: context
                                        .get(CLIENT_NAME)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                    client_version: context
                                        .get(CLIENT_VERSION)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                },
                                query_latency_stats: SingleQueryLatencyStats {
                                    latency: duration,
                                    has_errors,
                                    persisted_query_hit,
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                            referenced_fields_by_type: usage_reporting
                                .referenced_fields_by_type
                                .into_iter()
                                .map(|(k, v)| (k, convert(v)))
                                .collect(),
                        },
                    )]),
                }
            }
        } else {
            // Usage reporting was missing, so it counts as one operation.
            SingleReport {
                operation_count: 1,
                ..Default::default()
            }
        };
        sender.send(metrics);
    }

    async fn update_metrics(
        config: Arc<Conf>,
        context: Context,
        metrics: BasicMetrics,
        result: Result<RouterResponse, BoxError>,
        request_duration: Duration,
    ) -> Result<RouterResponse, BoxError> {
        let mut metric_attrs = context
            .get::<_, HashMap<String, String>>(ATTRIBUTES)
            .ok()
            .flatten()
            .map(|attrs| {
                attrs
                    .into_iter()
                    .map(|(attr_name, attr_value)| KeyValue::new(attr_name, attr_value))
                    .collect::<Vec<KeyValue>>()
            })
            .unwrap_or_default();
        let res = match result {
            Ok(response) => {
                metric_attrs.push(KeyValue::new(
                    "status",
                    response.response.status().as_u16().to_string(),
                ));
                if let Some(MetricsCommon {
                    attributes:
                        Some(MetricsAttributesConf {
                            router: Some(forward_attributes),
                            ..
                        }),
                    ..
                }) = &config.metrics.as_ref().and_then(|m| m.common.as_ref())
                {
                    let (resp, attributes) = forward_attributes
                        .get_attributes_from_router_response(response)
                        .await;

                    metric_attrs.extend(attributes.into_iter().map(|(k, v)| KeyValue::new(k, v)));
                    metrics.http_requests_total.add(1, &metric_attrs);

                    Ok(resp)
                } else {
                    metrics.http_requests_total.add(1, &metric_attrs);

                    Ok(response)
                }
            }
            Err(err) => {
                metrics.http_requests_error_total.add(1, &[]);

                Err(err)
            }
        };
        metrics
            .http_requests_duration
            .record(request_duration.as_secs_f64(), &metric_attrs);

        res
    }

    fn populate_context(config: Arc<Conf>, req: &RouterRequest) {
        let apollo_config = config.apollo.clone().unwrap_or_default();
        let context = &req.context;
        let http_request = &req.originating_request;
        let headers = http_request.headers();
        let client_name_header = &apollo_config.client_name_header;
        let client_version_header = &apollo_config.client_version_header;
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
        if let Some(metrics_conf) = &config.metrics {
            // List of custom attributes for metrics
            let mut attributes: HashMap<String, String> = HashMap::new();
            if let Some(operation_name) = &req.originating_request.body().operation_name {
                attributes.insert("operation_name".to_string(), operation_name.clone());
            }

            if let Some(router_attributes_conf) = metrics_conf
                .common
                .as_ref()
                .and_then(|c| c.attributes.as_ref())
                .and_then(|a| a.router.as_ref())
            {
                attributes.extend(
                    router_attributes_conf
                        .get_attributes_from_request(headers, req.originating_request.body()),
                );
                attributes.extend(router_attributes_conf.get_attributes_from_context(context));
            }

            let _ = context.insert(ATTRIBUTES, attributes);
        }
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
    referenced_fields: router_bridge::planner::ReferencedFieldsForType,
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

//
// Please ensure that any tests added to the tests module use the tokio multi-threaded test executor.
//
#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bytes::Bytes;
    use http::Method;
    use http::StatusCode;
    use http::Uri;
    use serde_json::Value;
    use serde_json_bytes::json;
    use serde_json_bytes::ByteString;
    use tower::util::BoxService;
    use tower::Service;
    use tower::ServiceExt;

    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::json_ext::Object;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::DynPlugin;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::RouterRequest;
    use crate::RouterResponse;

    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_registered() {
        crate::plugin::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({"apollo": {"schema_id":"abc"}, "tracing": {}}))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attribute_serialization() {
        crate::plugin::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({
                "apollo": {"schema_id":"abc"},
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
                },
                "metrics": {
                    "common": {
                        "attributes": {
                            "router": {
                                "static": [
                                    {
                                        "name": "myname",
                                        "value": "label_value"
                                    }
                                ],
                                "request": {
                                    "header": [{
                                        "named": "test",
                                        "default": "default_value",
                                        "rename": "renamed_value"
                                    }],
                                    "body": [{
                                        "path": ".data.test",
                                        "name": "my_new_name",
                                        "default": "default_value"
                                    }]
                                },
                                "response": {
                                    "header": [{
                                        "named": "test",
                                        "default": "default_value",
                                        "rename": "renamed_value",
                                    }, {
                                        "named": "test",
                                        "default": "default_value",
                                        "rename": "renamed_value",
                                    }],
                                    "body": [{
                                        "path": ".data.test",
                                        "name": "my_new_name",
                                        "default": "default_value"
                                    }]
                                }
                            },
                            "subgraph": {
                                "all": {
                                    "static": [
                                        {
                                            "name": "myname",
                                            "value": "label_value"
                                        }
                                    ],
                                    "request": {
                                        "header": [{
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value",
                                        }],
                                        "body": [{
                                            "path": ".data.test",
                                            "name": "my_new_name",
                                            "default": "default_value"
                                        }]
                                    },
                                    "response": {
                                        "header": [{
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value",
                                        }, {
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value",
                                        }],
                                        "body": [{
                                            "path": ".data.test",
                                            "name": "my_new_name",
                                            "default": "default_value"
                                        }]
                                    }
                                },
                                "subgraphs": {
                                    "subgraph_name_test": {
                                         "static": [
                                            {
                                                "name": "myname",
                                                "value": "label_value"
                                            }
                                        ],
                                        "request": {
                                            "header": [{
                                                "named": "test",
                                                "default": "default_value",
                                                "rename": "renamed_value",
                                            }],
                                            "body": [{
                                                "path": ".data.test",
                                                "name": "my_new_name",
                                                "default": "default_value"
                                            }]
                                        },
                                        "response": {
                                            "header": [{
                                                "named": "test",
                                                "default": "default_value",
                                                "rename": "renamed_value",
                                            }, {
                                                "named": "test",
                                                "default": "default_value",
                                                "rename": "renamed_value",
                                            }],
                                            "body": [{
                                                "path": ".data.test",
                                                "name": "my_new_name",
                                                "default": "default_value"
                                            }]
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics() {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                Ok(RouterResponse::fake_builder()
                    .context(req.context)
                    .header("x-custom", "coming_from_header")
                    .data(json!({"data": {"my_value": 2}}))
                    .build()
                    .unwrap())
            });

        let mut mock_subgraph_service = MockSubgraphService::new();
        mock_subgraph_service
            .expect_call()
            .times(1)
            .returning(move |req: SubgraphRequest| {
                let mut extension = Object::new();
                extension.insert(
                    serde_json_bytes::ByteString::from("status"),
                    serde_json_bytes::Value::String(ByteString::from("INTERNAL_SERVER_ERROR")),
                );
                let _ = req
                    .context
                    .insert("my_key", "my_custom_attribute_from_context".to_string())
                    .unwrap();
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .error(
                        Error::builder()
                            .message(String::from("an error occured"))
                            .extensions(extension)
                            .build(),
                    )
                    .build())
            });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(
                    r#"{
                "apollo": {
                    "client_name_header": "name_header",
                    "client_version_header": "version_header",
                    "schema_id": "schema_sha"
                },
                "metrics": {
                    "common": {
                        "resources": {
                            "service.name": "apollo-router"
                        },
                        "attributes": {
                            "router": {
                                "static": [
                                    {
                                        "name": "myname",
                                        "value": "label_value"
                                    }
                                ],
                                "request": {
                                    "header": [
                                        {
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value"
                                        },
                                        {
                                            "named": "another_test",
                                            "default": "my_default_value"
                                        }
                                    ]
                                },
                                "response": {
                                    "header": [{
                                        "named": "x-custom"
                                    }],
                                    "body": [{
                                        "path": ".data.data.my_value",
                                        "name": "my_value"
                                    }]
                                }
                            },
                            "subgraph": {
                                "subgraphs": {
                                    "my_subgraph_name": {
                                        "request": {
                                            "body": [{
                                                "path": ".query",
                                                "name": "query_from_request"
                                            }, {
                                                "path": ".data",
                                                "name": "unknown_data",
                                                "default": "default_value"
                                            }, {
                                                "path": ".data2",
                                                "name": "unknown_data_bis"
                                            }]
                                        },
                                        "response": {
                                            "body": [{
                                                "path": ".errors[0].extensions.status",
                                                "name": "error"
                                            }]
                                        },
                                        "context": [
                                            {
                                                "named": "my_key"
                                            }
                                        ]
                                    }
                                }
                            }
                        }
                    },
                    "prometheus": {
                        "enabled": true
                    }
                }
            }"#,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let mut router_service = dyn_plugin.router_service(BoxService::new(mock_service.build()));
        let router_req = RouterRequest::fake_builder().header("test", "my_value_set");

        let _router_response = router_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        let mut subgraph_service = dyn_plugin.subgraph_service(
            "my_subgraph_name",
            BoxService::new(mock_subgraph_service.build()),
        );
        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(
                http_ext::Request::fake_builder()
                    .header("test", "my_value_set")
                    .body(
                        Request::fake_builder()
                            .query(String::from("query { test }"))
                            .build(),
                    )
                    .build()
                    .unwrap(),
            )
            .build();
        let _subgraph_response = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();

        let handler = dyn_plugin.custom_endpoint().unwrap();
        let http_req_prom = http_ext::Request::fake_builder()
            .uri(Uri::from_static(
                "http://localhost:4000/BADPATH/apollo.telemetry/prometheus",
            ))
            .method(Method::GET)
            .body(Bytes::new())
            .build()
            .unwrap();
        let resp = handler.clone().oneshot(http_req_prom).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let http_req_prom = http_ext::Request::fake_builder()
            .uri(Uri::from_static(
                "http://localhost:4000/plugins/apollo.telemetry/prometheus",
            ))
            .method(Method::GET)
            .body(Bytes::new())
            .build()
            .unwrap();
        let resp = handler.oneshot(http_req_prom).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let prom_metrics = String::from_utf8_lossy(resp.body());
        assert!(prom_metrics.contains(r#"http_requests_total{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"} 1"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_count{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.001"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.005"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.015"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.05"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.3"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.4"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.5"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="1"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="5"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="10"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="+Inf"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_count{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_sum{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{error="INTERNAL_SERVER_ERROR",my_key="my_custom_attribute_from_context",query_from_request="query { test }",service_name="apollo-router",status="200",subgraph="my_subgraph_name",unknown_data="default_value",le="1"}"#));
    }
}
