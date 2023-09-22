//! Telemetry plugin.
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use ::tracing::field;
use ::tracing::info_span;
use ::tracing::Span;
use axum::headers::HeaderName;
use bloomfilter::Bloom;
use dashmap::DashMap;
use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::FutureExt;
use futures::StreamExt;
use http::header;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use multimap::MultiMap;
use once_cell::sync::OnceCell;
use opentelemetry::propagation::text_map_propagator::FieldIter;
use opentelemetry::propagation::Extractor;
use opentelemetry::propagation::Injector;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::metrics::controllers::BasicController;
use opentelemetry::sdk::propagation::TextMapCompositePropagator;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::trace::TracerProvider;
use opentelemetry::Context as OtelContext;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use rand::Rng;
use router_bridge::planner::UsageReporting;
use serde_json_bytes::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tokio::runtime::Handle;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::fmt::format::JsonFields;
use tracing_subscriber::Layer;

use self::apollo::ForwardValues;
use self::apollo::LicensedOperationCountByType;
use self::apollo::OperationSubType;
use self::apollo::SingleReport;
use self::apollo_exporter::proto;
use self::apollo_exporter::Sender;
use self::config::Conf;
use self::config::Sampler;
use self::config::SamplerOption;
use self::formatters::text::TextFormatter;
use self::metrics::apollo::studio::SingleTypeStat;
use self::metrics::AttributesForwardConf;
use self::metrics::MetricsAttributesConf;
use self::reload::reload_fmt;
use self::reload::reload_metrics;
use self::reload::LayeredTracer;
use self::reload::NullFieldFormatter;
use self::reload::SamplingFilter;
use self::reload::OPENTELEMETRY_TRACER_HANDLE;
use self::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use super::traffic_shaping::cache::hash_request;
use super::traffic_shaping::cache::hash_vary_headers;
use super::traffic_shaping::cache::REPRESENTATIONS;
use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::context::OPERATION_NAME;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::apollo::ForwardHeaders;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::node::Id::ResponseName;
use crate::plugins::telemetry::apollo_exporter::proto::reports::StatsContext;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config::Metrics;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::config::Tracing;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::metrics::aggregation::AggregateMeterProvider;
use crate::plugins::telemetry::metrics::apollo::studio::SingleContextualizedStats;
use crate::plugins::telemetry::metrics::apollo::studio::SinglePathErrorStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleQueryLatencyStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStatsReport;
use crate::plugins::telemetry::metrics::layer::MetricsLayer;
use crate::plugins::telemetry::metrics::BasicMetrics;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::metrics::MetricsExporterHandle;
use crate::plugins::telemetry::tracing::apollo_telemetry::decode_ftv1_trace;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::plugins::telemetry::utils::TracingUtils;
use crate::query_planner::OperationKind;
use crate::register_plugin;
use crate::router_factory::Endpoint;
use crate::services::execution;
use crate::services::router;
use crate::services::subgraph;
use crate::services::subgraph::Request;
use crate::services::subgraph::Response;
use crate::services::supergraph;
use crate::services::ExecutionRequest;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::TYPENAME;
use crate::tracer::TraceId;
use crate::Context;
use crate::ListenAddr;

pub(crate) mod apollo;
pub(crate) mod apollo_exporter;
pub(crate) mod config;
pub(crate) mod formatters;
pub(crate) mod metrics;
mod otlp;
pub(crate) mod reload;
pub(crate) mod tracing;
pub(crate) mod utils;

// Tracing consts
pub(crate) const SUPERGRAPH_SPAN_NAME: &str = "supergraph";
pub(crate) const SUBGRAPH_SPAN_NAME: &str = "subgraph";
pub(crate) const ROUTER_SPAN_NAME: &str = "router";
pub(crate) const EXECUTION_SPAN_NAME: &str = "execution";
const CLIENT_NAME: &str = "apollo_telemetry::client_name";
const CLIENT_VERSION: &str = "apollo_telemetry::client_version";
const SUBGRAPH_FTV1: &str = "apollo_telemetry::subgraph_ftv1";
pub(crate) const OPERATION_KIND: &str = "apollo_telemetry::operation_kind";
pub(crate) const STUDIO_EXCLUDE: &str = "apollo_telemetry::studio::exclude";
pub(crate) const LOGGING_DISPLAY_HEADERS: &str = "apollo_telemetry::logging::display_headers";
pub(crate) const LOGGING_DISPLAY_BODY: &str = "apollo_telemetry::logging::display_body";
const DEFAULT_SERVICE_NAME: &str = "apollo-router";
const GLOBAL_TRACER_NAME: &str = "apollo-router";
const DEFAULT_EXPOSE_TRACE_ID_HEADER: &str = "apollo-trace-id";
static DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME: HeaderName =
    HeaderName::from_static(DEFAULT_EXPOSE_TRACE_ID_HEADER);
static FTV1_HEADER_NAME: HeaderName = HeaderName::from_static("apollo-federation-include-trace");
static FTV1_HEADER_VALUE: HeaderValue = HeaderValue::from_static("ftv1");

#[doc(hidden)] // Only public for integration tests
pub(crate) struct Telemetry {
    config: Arc<config::Conf>,
    metrics: BasicMetrics,
    // Do not remove metrics_exporters. Metrics will not be exported if it is removed.
    // Typically the handles are a PushController but may be something else. Dropping the handle will
    // shutdown exporter.
    metrics_exporters: Vec<MetricsExporterHandle>,
    custom_endpoints: MultiMap<ListenAddr, Endpoint>,
    apollo_metrics_sender: apollo_exporter::Sender,
    field_level_instrumentation_ratio: f64,
    sampling_filter_ratio: SamplerOption,

    tracer_provider: Option<opentelemetry::sdk::trace::TracerProvider>,
    meter_provider: AggregateMeterProvider,
    counter: Option<Arc<Mutex<CacheCounter>>>,
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
        // If we can downcast the metrics exporter to be a `BasicController`, then we
        // should stop it to ensure metrics are transmitted before the exporter is dropped.
        for exporter in self.metrics_exporters.drain(..) {
            if let Ok(controller) = MetricsExporterHandle::downcast::<BasicController>(exporter) {
                ::tracing::debug!("stopping basic controller: {controller:?}");
                let cx = OtelContext::current();

                thread::spawn(move || {
                    if let Err(e) = controller.stop(&cx) {
                        ::tracing::error!("error during basic controller stop: {e}");
                    }
                    ::tracing::debug!("stopped basic controller: {controller:?}");
                });
            }
        }
        // If for some reason we didn't use the trace provider then safely discard it e.g. some other plugin failed `new`
        // To ensure we don't hang tracing providers are dropped in a blocking task.
        // https://github.com/open-telemetry/opentelemetry-rust/issues/868#issuecomment-1250387989
        // We don't have to worry about timeouts as every exporter is batched, which has a timeout on it already.
        if let Some(tracer_provider) = self.tracer_provider.take() {
            // If we have no runtime then we don't need to spawn a task as we are already in a blocking context.
            if Handle::try_current().is_ok() {
                // This is a thread for a reason!
                // Tokio doesn't finish executing tasks before termination https://github.com/tokio-rs/tokio/issues/1156.
                // This means that if the runtime is shutdown there is potentially a race where the provider may not be flushed.
                // By using a thread it doesn't matter if the tokio runtime is shut down.
                // This is likely to happen in tests due to the tokio runtime being destroyed when the test method exits.
                thread::spawn(move || drop(tracer_provider));
            }
        }
    }
}

#[async_trait::async_trait]
impl Plugin for Telemetry {
    type Config = config::Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let config = init.config;
        config.logging.validate()?;

        let field_level_instrumentation_ratio =
            config.calculate_field_level_instrumentation_ratio()?;
        let mut metrics_builder = Self::create_metrics_builder(&config)?;
        let meter_provider = metrics_builder.meter_provider();
        let counter = config
            .metrics
            .as_ref()
            .and_then(|m| m.common.as_ref())
            .and_then(|c| {
                if c.experimental_cache_metrics.enabled {
                    Some(Arc::new(Mutex::new(CacheCounter::new(
                        c.experimental_cache_metrics.ttl,
                    ))))
                } else {
                    None
                }
            });
        let (sampling_filter_ratio, tracer_provider) = Self::create_tracer_provider(&config)?;

        Ok(Telemetry {
            custom_endpoints: metrics_builder.custom_endpoints(),
            metrics_exporters: metrics_builder.exporters(),
            metrics: BasicMetrics::new(&meter_provider),
            apollo_metrics_sender: metrics_builder.apollo_metrics_provider(),
            field_level_instrumentation_ratio,
            tracer_provider: Some(tracer_provider),
            meter_provider,
            sampling_filter_ratio,
            config: Arc::new(config),
            counter,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let config = self.config.clone();
        let config_later = self.config.clone();

        ServiceBuilder::new()
            .map_response(|response: router::Response|{
                // The current span *should* be the request span as we are outside the instrument block.
                let span = Span::current();
                if let Some(REQUEST_SPAN_NAME) = span.metadata().map(|metadata| metadata.name()) {

                    //https://opentelemetry.io/docs/specs/otel/trace/semantic_conventions/instrumentation/graphql/
                    let operation_kind = response.context.get::<_, String>(OPERATION_KIND);
                    let operation_name = response.context.get::<_, String>(OPERATION_NAME);

                    if let Ok(Some(operation_kind)) = &operation_kind {
                        span.record("graphql.operation.type", operation_kind);
                    }
                    if let Ok(Some(operation_name)) = &operation_name {
                        span.record("graphql.operation.name", operation_name);
                    }
                    match (&operation_kind, &operation_name) {
                        (Ok(Some(kind)), Ok(Some(name))) => span.record("otel.name", format!("{kind} {name}")),
                        (Ok(Some(kind)), _) => span.record("otel.name", kind),
                        _ => span.record("otel.name", "GraphQL Operation")
                    };
                }

                response
            })
            .instrument(move |request: &router::Request| {
                let apollo = config.apollo.as_ref().cloned().unwrap_or_default();
                let trace_id = TraceId::maybe_new()
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                let router_request = &request.router_request;
                let headers = router_request.headers();
                let client_name: &str = headers
                    .get(&apollo.client_name_header)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                let client_version = headers
                    .get(&apollo.client_version_header)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                let span = ::tracing::info_span!(ROUTER_SPAN_NAME,
                    "http.method" = %router_request.method(),
                    "http.route" = %router_request.uri(),
                    "http.flavor" = ?router_request.version(),
                    "trace_id" = %trace_id,
                    "client.name" = client_name,
                    "client.version" = client_version,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = ::tracing::field::Empty,
                    "apollo_private.duration_ns" = ::tracing::field::Empty,
                    "apollo_private.http.request_headers" = filter_headers(request.router_request.headers(), &apollo.send_headers).as_str(),
                    "apollo_private.http.response_headers" = field::Empty
                );
                span
            })
            .map_future(move |fut| {
                let start = Instant::now();
                let config = config_later.clone();

                Self::plugin_metrics(&config);


                async move {
                    let span = Span::current();
                    let response: Result<router::Response, BoxError> = fut.await;

                    span.record(
                        APOLLO_PRIVATE_DURATION_NS,
                        start.elapsed().as_nanos() as i64,
                    );


                    let expose_trace_id = config.tracing.as_ref().cloned().unwrap_or_default().response_trace_id;
                    if let Ok(response) = &response {
                        if expose_trace_id.enabled {
                            if let Some(header_name) = &expose_trace_id.header_name {
                                let mut headers: HashMap<String, Vec<String>> = HashMap::new();
                                if let Some(value) = response.response.headers().get(header_name) {
                                    headers.insert(header_name.to_string(), vec![value.to_str().unwrap_or_default().to_string()]);
                                    let response_headers = serde_json::to_string(&headers).unwrap_or_default();
                                    span.record("apollo_private.http.response_headers",&response_headers);
                                }
                            }
                        }

                        if response.response.status() >= StatusCode::BAD_REQUEST {
                            span.record("otel.status_code", "Error");
                        } else {
                            span.record("otel.status_code", "Ok");
                        }

                    }
                    response
                }
            })
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let metrics_sender = self.apollo_metrics_sender.clone();
        let metrics = self.metrics.clone();
        let config = self.config.clone();
        let config_map_res_first = config.clone();
        let config_map_res = config.clone();
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        ServiceBuilder::new()
            .instrument(Self::supergraph_service_span(
                self.field_level_instrumentation_ratio,
                config.apollo.clone().unwrap_or_default(),
            ))
            .map_response(move |mut resp: SupergraphResponse| {
                let config = config_map_res_first.clone();
                if let Some(usage_reporting) =
                    resp.context.private_entries.lock().get::<UsageReporting>()
                {
                    // Record the operation signature on the router span
                    Span::current().record(
                        APOLLO_PRIVATE_OPERATION_SIGNATURE.as_str(),
                        usage_reporting.stats_report_key.as_str(),
                    );
                }
                // To expose trace_id or not
                let expose_trace_id_header = config.tracing.as_ref().and_then(|t| {
                    t.response_trace_id.enabled.then(|| {
                        t.response_trace_id
                            .header_name
                            .clone()
                            .unwrap_or_else(||DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME.clone())
                    })
                });
                if let (Some(header_name), Some(trace_id)) = (
                    expose_trace_id_header,
                    TraceId::maybe_new().and_then(|t| HeaderValue::from_str(&t.to_string()).ok()),
                ) {
                    resp.response.headers_mut().append(header_name, trace_id);
                }

                if resp.context.contains_key(LOGGING_DISPLAY_HEADERS) {
                    ::tracing::info!(http.response.headers = ?resp.response.headers(), "Supergraph response headers");
                }
                let display_body = resp.context.contains_key(LOGGING_DISPLAY_BODY);
                resp.map_stream(move |gql_response| {
                    if display_body {
                        ::tracing::info!(http.response.body = ?gql_response, "Supergraph GraphQL response");
                    }
                    gql_response
                })
            })
            .map_future_with_request_data(
                move |req: &SupergraphRequest| {
                    Self::populate_context(config.clone(), field_level_instrumentation_ratio, req);
                    req.context.clone()
                },
                move |ctx: Context, fut| {
                    let config = config_map_res.clone();
                    let metrics = metrics.clone();
                    let sender = metrics_sender.clone();
                    let start = Instant::now();

                    async move {
                        let mut result: Result<SupergraphResponse, BoxError> = fut.await;
                        result = Self::update_otel_metrics(
                            config.clone(),
                            ctx.clone(),
                            metrics.clone(),
                            result,
                            start.elapsed(),
                        )
                        .await;
                        Self::update_metrics_on_response_events(
                            &ctx, config, field_level_instrumentation_ratio, metrics, sender, start, result,
                        )
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .instrument(move |req: &ExecutionRequest| {
                let operation_kind = req
                    .query_plan
                    .query
                    .operation(req.supergraph_request.body().operation_name.as_deref())
                    .map(|op| *op.kind());
                let _ = req
                    .context
                    .insert(OPERATION_KIND, operation_kind.unwrap_or_default());

                match operation_kind {
                    Some(operation_kind) => {
                        info_span!(
                            EXECUTION_SPAN_NAME,
                            "otel.kind" = "INTERNAL",
                            "graphql.operation.type" = operation_kind.as_apollo_operation_type()
                        )
                    }
                    None => {
                        info_span!(EXECUTION_SPAN_NAME, "otel.kind" = "INTERNAL",)
                    }
                }
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let metrics = self.metrics.clone();
        let subgraph_attribute = KeyValue::new("subgraph", name.to_string());
        let subgraph_metrics_conf_req = self.create_subgraph_metrics_conf(name);
        let subgraph_metrics_conf_resp = subgraph_metrics_conf_req.clone();
        let subgraph_name = ByteString::from(name);
        let cache_metrics_enabled = self.counter.is_some();
        let counter = self.counter.clone();
        let name = name.to_owned();
        let subgraph_name_arc = Arc::new(name.to_owned());
        ServiceBuilder::new()
            .instrument(move |req: &SubgraphRequest| {
                let query = req
                    .subgraph_request
                    .body()
                    .query
                    .as_deref()
                    .unwrap_or_default();
                let operation_name = req
                    .subgraph_request
                    .body()
                    .operation_name
                    .as_deref()
                    .unwrap_or_default();

                info_span!(
                    SUBGRAPH_SPAN_NAME,
                    "apollo.subgraph.name" = name.as_str(),
                    graphql.document = query,
                    graphql.operation.name = operation_name,
                    "otel.kind" = "INTERNAL",
                    "apollo_private.ftv1" = field::Empty
                )
            })
            .map_request(move |mut req: SubgraphRequest| {
                let cache_attributes = cache_metrics_enabled
                    .then(|| Self::get_cache_attributes(subgraph_name_arc.clone(), &mut req))
                    .flatten();
                if let Some(cache_attributes) = cache_attributes {
                    req.context.private_entries.lock().insert(cache_attributes);
                }

                request_ftv1(req)
            })
            .map_response(move |resp| store_ftv1(&subgraph_name, resp))
            .map_future_with_request_data(
                move |sub_request: &SubgraphRequest| {
                    Self::store_subgraph_request_attributes(
                        subgraph_metrics_conf_req.clone(),
                        sub_request,
                    );
                    let cache_attributes = sub_request.context.private_entries.lock().remove();

                    (sub_request.context.clone(), cache_attributes)
                },
                move |(context, cache_attributes): (Context, Option<CacheAttributes>),
                      f: BoxFuture<'static, Result<SubgraphResponse, BoxError>>| {
                    let metrics = metrics.clone();
                    let subgraph_attribute = subgraph_attribute.clone();
                    let subgraph_metrics_conf = subgraph_metrics_conf_resp.clone();
                    let counter = counter.clone();
                    // Using Instant because it is guaranteed to be monotonically increasing.
                    let now = Instant::now();
                    f.map(move |result: Result<SubgraphResponse, BoxError>| {
                        Self::store_subgraph_response_attributes(
                            &context,
                            metrics,
                            subgraph_attribute,
                            subgraph_metrics_conf,
                            now,
                            counter,
                            cache_attributes,
                            &result,
                        );
                        result
                    })
                },
            )
            .service(service)
            .boxed()
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        self.custom_endpoints.clone()
    }
}

impl Telemetry {
    pub(crate) fn activate(&mut self) {
        // Only apply things if we were executing in the context of a vanilla the Apollo executable.
        // Users that are rolling their own routers will need to set up telemetry themselves.
        if let Some(hot_tracer) = OPENTELEMETRY_TRACER_HANDLE.get() {
            SamplingFilter::configure(&self.sampling_filter_ratio);

            // The reason that this has to happen here is that we are interacting with global state.
            // If we do this logic during plugin init then if a subsequent plugin fails to init then we
            // will already have set the new tracer provider and we will be in an inconsistent state.
            // activate is infallible, so if we get here we know the new pipeline is ready to go.
            let tracer_provider = self
                .tracer_provider
                .take()
                .expect("must have new tracer_provider");

            let tracer = tracer_provider.versioned_tracer(
                GLOBAL_TRACER_NAME,
                Some(env!("CARGO_PKG_VERSION")),
                None,
            );
            hot_tracer.reload(tracer);

            let last_provider = opentelemetry::global::set_tracer_provider(tracer_provider);
            // To ensure we don't hang tracing providers are dropped in a blocking task.
            // https://github.com/open-telemetry/opentelemetry-rust/issues/868#issuecomment-1250387989
            // We don't have to worry about timeouts as every exporter is batched, which has a timeout on it already.
            tokio::task::spawn_blocking(move || drop(last_provider));
            opentelemetry::global::set_error_handler(handle_error)
                .expect("otel error handler lock poisoned, fatal");

            opentelemetry::global::set_text_map_propagator(Self::create_propagator(&self.config));
        }

        reload_metrics(MetricsLayer::new(&self.meter_provider));
        reload_fmt(Self::create_fmt_layer(&self.config));
    }

    fn create_propagator(config: &config::Conf) -> TextMapCompositePropagator {
        let propagation = config
            .clone()
            .tracing
            .and_then(|c| c.propagation)
            .unwrap_or_default();

        let tracing = config.clone().tracing.unwrap_or_default();

        let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
        // TLDR the jaeger propagator MUST BE the first one because the version of opentelemetry_jaeger is buggy.
        // It overrides the current span context with an empty one if it doesn't find the corresponding headers.
        // Waiting for the >=0.16.1 release
        if propagation.jaeger || tracing.jaeger.is_some() {
            propagators.push(Box::<opentelemetry_jaeger::Propagator>::default());
        }
        if propagation.baggage {
            propagators.push(Box::<opentelemetry::sdk::propagation::BaggagePropagator>::default());
        }
        if propagation.trace_context || tracing.otlp.is_some() {
            propagators
                .push(Box::<opentelemetry::sdk::propagation::TraceContextPropagator>::default());
        }
        if propagation.zipkin || tracing.zipkin.is_some() {
            propagators.push(Box::<opentelemetry_zipkin::Propagator>::default());
        }
        if propagation.datadog || tracing.datadog.is_some() {
            propagators.push(Box::<opentelemetry_datadog::DatadogPropagator>::default());
        }
        if propagation.awsxray {
            propagators.push(Box::<opentelemetry_aws::XrayPropagator>::default());
        }
        if let Some(from_request_header) = &propagation.request.header_name {
            propagators.push(Box::new(CustomTraceIdPropagator::new(
                from_request_header.to_string(),
            )));
        }

        TextMapCompositePropagator::new(propagators)
    }

    fn create_tracer_provider(
        config: &config::Conf,
    ) -> Result<(SamplerOption, opentelemetry::sdk::trace::TracerProvider), BoxError> {
        let tracing_config = config.tracing.clone().unwrap_or_default();
        let mut trace_config = tracing_config.trace_config.unwrap_or_default();
        let mut sampler = trace_config.sampler;
        // set it to AlwaysOn: it is now done in the SamplingFilter, so whatever is sent to an exporter
        // should be accepted
        trace_config.sampler = SamplerOption::Always(Sampler::AlwaysOn);

        let mut builder = opentelemetry::sdk::trace::TracerProvider::builder()
            .with_config((&trace_config).into());

        builder = setup_tracing(builder, &tracing_config.jaeger, &trace_config)?;
        builder = setup_tracing(builder, &tracing_config.zipkin, &trace_config)?;
        builder = setup_tracing(builder, &tracing_config.datadog, &trace_config)?;
        builder = setup_tracing(builder, &tracing_config.otlp, &trace_config)?;
        builder = setup_tracing(builder, &config.apollo, &trace_config)?;

        if tracing_config.jaeger.is_none()
            && tracing_config.zipkin.is_none()
            && tracing_config.datadog.is_none()
            && tracing_config.otlp.is_none()
            && config.apollo.is_none()
        {
            sampler = SamplerOption::Always(Sampler::AlwaysOff);
        }

        let tracer_provider = builder.build();
        Ok((sampler, tracer_provider))
    }

    fn create_metrics_builder(config: &config::Conf) -> Result<MetricsBuilder, BoxError> {
        let metrics_config = config.metrics.clone().unwrap_or_default();
        let metrics_common_config = &mut metrics_config.common.unwrap_or_default();
        // Set default service name for metrics
        if metrics_common_config
            .resources
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME.as_str())
            .is_none()
        {
            metrics_common_config.resources.insert(
                String::from(opentelemetry_semantic_conventions::resource::SERVICE_NAME.as_str()),
                String::from(
                    metrics_common_config
                        .service_name
                        .as_deref()
                        .unwrap_or(DEFAULT_SERVICE_NAME),
                ),
            );
        }
        if let Some(service_namespace) = &metrics_common_config.service_namespace {
            metrics_common_config.resources.insert(
                String::from(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE.as_str(),
                ),
                service_namespace.clone(),
            );
        }

        let mut builder = MetricsBuilder::default();
        builder = setup_metrics_exporter(builder, &config.apollo, metrics_common_config)?;
        builder =
            setup_metrics_exporter(builder, &metrics_config.prometheus, metrics_common_config)?;
        builder = setup_metrics_exporter(builder, &metrics_config.otlp, metrics_common_config)?;
        Ok(builder)
    }

    fn create_fmt_layer(config: &config::Conf) -> Box<dyn Layer<LayeredTracer> + Send + Sync> {
        let logging = &config.logging;
        let fmt = match logging.format {
            config::LoggingFormat::Pretty => tracing_subscriber::fmt::layer()
                .event_format(FilteringFormatter::new(
                    TextFormatter::new()
                        .with_filename(logging.display_filename)
                        .with_line(logging.display_line_number)
                        .with_target(logging.display_target),
                    filter_metric_events,
                ))
                .fmt_fields(NullFieldFormatter)
                .boxed(),
            config::LoggingFormat::Json => tracing_subscriber::fmt::layer()
                .json()
                .with_file(logging.display_filename)
                .with_line_number(logging.display_line_number)
                .with_target(logging.display_target)
                .map_event_format(|e| {
                    FilteringFormatter::new(
                        e.json()
                            .with_current_span(true)
                            .with_span_list(true)
                            .flatten_event(true),
                        filter_metric_events,
                    )
                })
                .fmt_fields(NullFieldFormatter)
                .map_fmt_fields(|_f| JsonFields::default())
                .boxed(),
        };
        fmt
    }

    fn supergraph_service_span(
        field_level_instrumentation_ratio: f64,
        config: apollo::Config,
    ) -> impl Fn(&SupergraphRequest) -> Span + Clone {
        move |request: &SupergraphRequest| {
            let http_request = &request.supergraph_request;
            let query = http_request.body().query.as_deref().unwrap_or_default();
            let span = info_span!(
                SUPERGRAPH_SPAN_NAME,
                graphql.document = query,
                // TODO add graphql.operation.type
                graphql.operation.name = field::Empty,
                otel.kind = "INTERNAL",
                apollo_private.field_level_instrumentation_ratio =
                    field_level_instrumentation_ratio,
                apollo_private.operation_signature = field::Empty,
                apollo_private.graphql.variables = Self::filter_variables_values(
                    &request.supergraph_request.body().variables,
                    &config.send_variable_values,
                ),
            );
            if let Some(operation_name) = request
                .context
                .get::<_, String>(OPERATION_NAME)
                .unwrap_or_default()
            {
                span.record("graphql.operation.name", operation_name);
            }

            span
        }
    }

    fn filter_variables_values(
        variables: &Map<ByteString, Value>,
        forward_rules: &ForwardValues,
    ) -> String {
        #[allow(clippy::mutable_key_type)] // False positive lint
        let variables = variables
            .iter()
            .map(|(name, value)| {
                if match &forward_rules {
                    ForwardValues::None => false,
                    ForwardValues::All => true,
                    ForwardValues::Only(only) => only.contains(&name.as_str().to_string()),
                    ForwardValues::Except(except) => !except.contains(&name.as_str().to_string()),
                } {
                    (
                        name,
                        serde_json::to_string(value).unwrap_or_else(|_| "<unknown>".to_string()),
                    )
                } else {
                    (name, "".to_string())
                }
            })
            .fold(BTreeMap::new(), |mut acc, (name, value)| {
                acc.insert(name, value);
                acc
            });

        match serde_json::to_string(&variables) {
            Ok(result) => result,
            Err(_err) => {
                ::tracing::warn!(
                    "could not serialize variables, trace will not have variables information"
                );
                Default::default()
            }
        }
    }

    async fn update_otel_metrics(
        config: Arc<Conf>,
        context: Context,
        metrics: BasicMetrics,
        result: Result<SupergraphResponse, BoxError>,
        request_duration: Duration,
    ) -> Result<SupergraphResponse, BoxError> {
        let mut metric_attrs = {
            context
                .private_entries
                .lock()
                .get::<MetricsAttributes>()
                .cloned()
        }
        .map(|attrs| {
            attrs
                .0
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

                // Wait for the first response of the stream
                let (parts, stream) = response.response.into_parts();
                let (first_response, rest) = stream.into_future().await;

                if let Some(MetricsCommon {
                    attributes:
                        Some(MetricsAttributesConf {
                            supergraph: Some(forward_attributes),
                            ..
                        }),
                    ..
                }) = &config.metrics.as_ref().and_then(|m| m.common.as_ref())
                {
                    let attributes = forward_attributes.get_attributes_from_router_response(
                        &parts,
                        &context,
                        &first_response,
                    );

                    metric_attrs.extend(attributes.into_iter().map(|(k, v)| KeyValue::new(k, v)));
                }

                if !parts.status.is_success() {
                    metric_attrs.push(KeyValue::new("error", parts.status.to_string()));
                }
                ::tracing::info!(
                    monotonic_counter.apollo.router.operations = 1u64,
                    http.response.status_code = parts.status.as_u16() as i64,
                );
                let response = http::Response::from_parts(
                    parts,
                    once(ready(first_response.unwrap_or_default()))
                        .chain(rest)
                        .boxed(),
                );

                Ok(SupergraphResponse { context, response })
            }
            Err(err) => {
                metric_attrs.push(KeyValue::new("status", "500"));

                ::tracing::info!(
                    monotonic_counter.apollo.router.operations = 1u64,
                    http.response.status_code = 500i64,
                );
                Err(err)
            }
        };

        // http_requests_total - the total number of HTTP requests received
        metrics
            .http_requests_total
            .add(&opentelemetry::Context::current(), 1, &metric_attrs);

        metrics.http_requests_duration.record(
            &opentelemetry::Context::current(),
            request_duration.as_secs_f64(),
            &metric_attrs,
        );

        res
    }

    fn populate_context(
        config: Arc<Conf>,
        field_level_instrumentation_ratio: f64,
        req: &SupergraphRequest,
    ) {
        let apollo_config = config.apollo.clone().unwrap_or_default();
        let context = &req.context;
        let http_request = &req.supergraph_request;
        let headers = http_request.headers();
        let client_name_header = &apollo_config.client_name_header;
        let client_version_header = &apollo_config.client_version_header;
        if let Some(name) = headers
            .get(client_name_header)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_owned())
        {
            let _ = context.insert(CLIENT_NAME, name);
        }

        if let Some(version) = headers
            .get(client_version_header)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_owned())
        {
            let _ = context.insert(CLIENT_VERSION, version);
        }

        let (should_log_headers, should_log_body) = config.logging.should_log(req);
        if should_log_headers {
            ::tracing::info!(http.request.headers = ?req.supergraph_request.headers(), "Supergraph request headers");

            let _ = req.context.insert(LOGGING_DISPLAY_HEADERS, true);
        }
        if should_log_body {
            ::tracing::info!(http.request.body = ?req.supergraph_request.body(), "Supergraph request body");

            let _ = req.context.insert(LOGGING_DISPLAY_BODY, true);
        }

        if let Some(metrics_conf) = &config.metrics {
            // List of custom attributes for metrics
            let mut attributes: HashMap<String, AttributeValue> = HashMap::new();
            if let Some(operation_name) = &req.supergraph_request.body().operation_name {
                attributes.insert(
                    OPERATION_NAME.to_string(),
                    AttributeValue::String(operation_name.clone()),
                );
            }

            if let Some(router_attributes_conf) = metrics_conf
                .common
                .as_ref()
                .and_then(|c| c.attributes.as_ref())
                .and_then(|a| a.supergraph.as_ref())
            {
                attributes.extend(
                    router_attributes_conf
                        .get_attributes_from_request(headers, req.supergraph_request.body()),
                );
                attributes.extend(router_attributes_conf.get_attributes_from_context(context));
            }

            let _ = context
                .private_entries
                .lock()
                .insert(MetricsAttributes(attributes));
        }
        if rand::thread_rng().gen_bool(field_level_instrumentation_ratio) {
            context.private_entries.lock().insert(EnableSubgraphFtv1);
        }
    }

    fn create_subgraph_metrics_conf(&self, name: &str) -> Arc<Option<AttributesForwardConf>> {
        Arc::new(
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
                                        .get(&name.to_owned())
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
                                        .get(&name.to_owned())
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
                    let errors = merge_config!(errors);

                    AttributesForwardConf {
                        insert: (!insert.is_empty()).then_some(insert),
                        request: (request.header.is_some() || request.body.is_some())
                            .then_some(request),
                        response: (response.header.is_some() || response.body.is_some())
                            .then_some(response),
                        errors: (errors.extensions.is_some() || errors.include_messages)
                            .then_some(errors),
                        context: (!context.is_empty()).then_some(context),
                    }
                }),
        )
    }

    fn get_cache_attributes(
        subgraph_name: Arc<String>,
        sub_request: &mut Request,
    ) -> Option<CacheAttributes> {
        let body = dbg!(sub_request.subgraph_request.body_mut());
        let hashed_query = hash_request(body);
        let representations = body
            .variables
            .get(REPRESENTATIONS)
            .and_then(|value| value.as_array())?;

        let keys = extract_cache_attributes(representations).ok()?;

        Some(CacheAttributes {
            subgraph_name,
            headers: sub_request.subgraph_request.headers().clone(),
            hashed_query: Arc::new(hashed_query),
            representations: keys,
        })
    }

    fn update_cache_metrics(
        counter: Arc<Mutex<CacheCounter>>,
        sub_response: &SubgraphResponse,
        cache_attributes: CacheAttributes,
    ) {
        let mut vary_headers = sub_response
            .response
            .headers()
            .get_all(header::VARY)
            .into_iter()
            .filter_map(|val| {
                val.to_str().ok().map(|v| {
                    v.to_string()
                        .split(", ")
                        .map(|s| s.to_string())
                        .collect::<Vec<String>>()
                })
            })
            .flatten()
            .collect::<Vec<String>>();
        vary_headers.sort();
        let vary_headers = vary_headers.join(", ");

        let hashed_headers = if vary_headers.is_empty() {
            Arc::default()
        } else {
            Arc::new(hash_vary_headers(&cache_attributes.headers))
        };
        counter.lock().record(
            cache_attributes.hashed_query.clone(),
            cache_attributes.subgraph_name.clone(),
            hashed_headers,
            cache_attributes.representations,
        );
    }

    fn store_subgraph_request_attributes(
        attribute_forward_config: Arc<Option<AttributesForwardConf>>,
        sub_request: &Request,
    ) {
        let mut attributes = HashMap::new();
        if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
            attributes.extend(subgraph_attributes_conf.get_attributes_from_request(
                sub_request.subgraph_request.headers(),
                sub_request.subgraph_request.body(),
            ));
            attributes
                .extend(subgraph_attributes_conf.get_attributes_from_context(&sub_request.context));
        }
        sub_request
            .context
            .private_entries
            .lock()
            .insert(SubgraphMetricsAttributes(attributes)); //.unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn store_subgraph_response_attributes(
        context: &Context,
        metrics: BasicMetrics,
        subgraph_attribute: KeyValue,
        attribute_forward_config: Arc<Option<AttributesForwardConf>>,
        now: Instant,
        counter: Option<Arc<Mutex<CacheCounter>>>,
        cache_attributes: Option<CacheAttributes>,
        result: &Result<Response, BoxError>,
    ) {
        let mut metric_attrs = {
            context
                .private_entries
                .lock()
                .get::<SubgraphMetricsAttributes>()
                .cloned()
        }
        .map(|attrs| {
            attrs
                .0
                .into_iter()
                .map(|(attr_name, attr_value)| KeyValue::new(attr_name, attr_value))
                .collect::<Vec<KeyValue>>()
        })
        .unwrap_or_default();
        metric_attrs.push(subgraph_attribute);
        // Fill attributes from context
        if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
            metric_attrs.extend(
                subgraph_attributes_conf
                    .get_attributes_from_context(context)
                    .into_iter()
                    .map(|(k, v)| KeyValue::new(k, v)),
            );
        }

        match &result {
            Ok(response) => {
                if let Some(cache_attributes) = cache_attributes {
                    if let Ok(cache_control) = response
                        .response
                        .headers()
                        .get(header::CACHE_CONTROL)
                        .ok_or(())
                        .and_then(|val| val.to_str().map(|v| v.to_string()).map_err(|_| ()))
                    {
                        metric_attrs.push(KeyValue::new("cache_control", cache_control));
                    }

                    if let Some(counter) = counter {
                        Self::update_cache_metrics(counter, response, cache_attributes)
                    }
                }
                metric_attrs.push(KeyValue::new(
                    "status",
                    response.response.status().as_u16().to_string(),
                ));

                // Fill attributes from response
                if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
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

                metrics.http_requests_total.add(
                    &opentelemetry::Context::current(),
                    1,
                    &metric_attrs,
                );
            }
            Err(err) => {
                metric_attrs.push(KeyValue::new("status", "500"));
                // Fill attributes from error
                if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
                    metric_attrs.extend(
                        subgraph_attributes_conf
                            .get_attributes_from_error(err)
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    );
                }

                metrics.http_requests_total.add(
                    &opentelemetry::Context::current(),
                    1,
                    &metric_attrs,
                );
            }
        }
        metrics.http_requests_duration.record(
            &opentelemetry::Context::current(),
            now.elapsed().as_secs_f64(),
            &metric_attrs,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn update_metrics_on_response_events(
        ctx: &Context,
        config: Arc<Conf>,
        field_level_instrumentation_ratio: f64,
        metrics: BasicMetrics,
        sender: Sender,
        start: Instant,
        result: Result<supergraph::Response, BoxError>,
    ) -> Result<supergraph::Response, BoxError> {
        let operation_kind: OperationKind =
            ctx.get(OPERATION_KIND).ok().flatten().unwrap_or_default();

        match result {
            Err(e) => {
                if !matches!(sender, Sender::Noop) {
                    let operation_subtype = (operation_kind == OperationKind::Subscription)
                        .then_some(OperationSubType::SubscriptionRequest);
                    Self::update_apollo_metrics(
                        ctx,
                        field_level_instrumentation_ratio,
                        sender,
                        true,
                        start.elapsed(),
                        operation_kind,
                        operation_subtype,
                    );
                }
                let mut metric_attrs = Vec::new();
                // Fill attributes from error
                if let Some(subgraph_attributes_conf) = config
                    .metrics
                    .as_ref()
                    .and_then(|m| m.common.as_ref())
                    .and_then(|c| c.attributes.as_ref())
                    .and_then(|c| c.supergraph.as_ref())
                {
                    metric_attrs.extend(
                        subgraph_attributes_conf
                            .get_attributes_from_error(&e)
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    );
                }

                metrics.http_requests_total.add(
                    &opentelemetry::Context::current(),
                    1,
                    &metric_attrs,
                );

                Err(e)
            }
            Ok(router_response) => {
                let http_status_is_success = router_response.response.status().is_success();

                // Only send the subscription-request metric if it's an http status in error because we won't always enter the stream after.
                if operation_kind == OperationKind::Subscription && !http_status_is_success {
                    Self::update_apollo_metrics(
                        ctx,
                        field_level_instrumentation_ratio,
                        sender.clone(),
                        true,
                        start.elapsed(),
                        operation_kind,
                        Some(OperationSubType::SubscriptionRequest),
                    );
                }
                Ok(router_response.map(move |response_stream| {
                    let sender = sender.clone();
                    let ctx = ctx.clone();

                    response_stream
                        .enumerate()
                        .map(move |(idx, response)| {
                            let has_errors = !response.errors.is_empty();

                            if !matches!(sender, Sender::Noop) {
                                if operation_kind == OperationKind::Subscription {
                                    // The first empty response is always a heartbeat except if it's an error
                                    if idx == 0 {
                                        // Don't count for subscription-request if http status was in error because it has been counted before
                                        if http_status_is_success {
                                            Self::update_apollo_metrics(
                                                &ctx,
                                                field_level_instrumentation_ratio,
                                                sender.clone(),
                                                has_errors,
                                                start.elapsed(),
                                                operation_kind,
                                                Some(OperationSubType::SubscriptionRequest),
                                            );
                                        }
                                    } else {
                                        // Only for subscription events
                                        Self::update_apollo_metrics(
                                            &ctx,
                                            field_level_instrumentation_ratio,
                                            sender.clone(),
                                            has_errors,
                                            response
                                                .created_at
                                                .map(|c| c.elapsed())
                                                .unwrap_or_else(|| start.elapsed()),
                                            operation_kind,
                                            Some(OperationSubType::SubscriptionEvent),
                                        );
                                    }
                                } else {
                                    // If it's the last response
                                    if !response.has_next.unwrap_or(false) {
                                        Self::update_apollo_metrics(
                                            &ctx,
                                            field_level_instrumentation_ratio,
                                            sender.clone(),
                                            has_errors,
                                            start.elapsed(),
                                            operation_kind,
                                            None,
                                        );
                                    }
                                }
                            }

                            response
                        })
                        .boxed()
                }))
            }
        }
    }

    fn update_apollo_metrics(
        context: &Context,
        field_level_instrumentation_ratio: f64,
        sender: Sender,
        has_errors: bool,
        duration: Duration,
        operation_kind: OperationKind,
        operation_subtype: Option<OperationSubType>,
    ) {
        let metrics = if let Some(usage_reporting) = context
            .private_entries
            .lock()
            .get::<UsageReporting>()
            .cloned()
        {
            let licensed_operation_count =
                licensed_operation_count(&usage_reporting.stats_report_key);
            let persisted_query_hit = context
                .get::<_, bool>("persisted_query_hit")
                .unwrap_or_default();

            if context
                .get(STUDIO_EXCLUDE)
                .map_or(false, |x| x.unwrap_or_default())
            {
                // The request was excluded don't report the details, but do report the operation count
                SingleStatsReport {
                    licensed_operation_count_by_type: (licensed_operation_count > 0).then_some(
                        LicensedOperationCountByType {
                            r#type: operation_kind,
                            subtype: operation_subtype,
                            licensed_operation_count,
                        },
                    ),
                    ..Default::default()
                }
            } else {
                let traces = Self::subgraph_ftv1_traces(context);
                let per_type_stat = Self::per_type_stat(&traces, field_level_instrumentation_ratio);
                let root_error_stats = Self::per_path_error_stats(&traces);
                SingleStatsReport {
                    request_id: uuid::Uuid::from_bytes(
                        Span::current()
                            .context()
                            .span()
                            .span_context()
                            .trace_id()
                            .to_bytes(),
                    ),
                    licensed_operation_count_by_type: (licensed_operation_count > 0).then_some(
                        LicensedOperationCountByType {
                            r#type: operation_kind,
                            subtype: operation_subtype,
                            licensed_operation_count,
                        },
                    ),
                    stats: HashMap::from([(
                        usage_reporting.stats_report_key.to_string(),
                        SingleStats {
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
                                    operation_type: operation_kind
                                        .as_apollo_operation_type()
                                        .to_string(),
                                    operation_subtype: operation_subtype
                                        .map(|op| op.to_string())
                                        .unwrap_or_default(),
                                },
                                query_latency_stats: SingleQueryLatencyStats {
                                    latency: duration,
                                    has_errors,
                                    persisted_query_hit,
                                    root_error_stats,
                                    ..Default::default()
                                },
                                per_type_stat,
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
            SingleStatsReport {
                licensed_operation_count_by_type: LicensedOperationCountByType {
                    r#type: operation_kind,
                    subtype: operation_subtype,
                    licensed_operation_count: 1,
                }
                .into(),
                ..Default::default()
            }
        };
        sender.send(SingleReport::Stats(metrics));
    }

    /// Returns `[(subgraph_name, trace), …]`
    fn subgraph_ftv1_traces(context: &Context) -> Vec<(ByteString, proto::reports::Trace)> {
        if let Some(Value::Array(array)) = context.get_json_value(SUBGRAPH_FTV1) {
            array
                .iter()
                .filter_map(|value| match value.as_array()?.as_slice() {
                    [Value::String(subgraph_name), trace] => {
                        Some((subgraph_name.clone(), decode_ftv1_trace(trace.as_str()?)?))
                    }
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    // https://github.com/apollographql/apollo-server/blob/6ff88e87c52/packages/server/src/plugin/usageReporting/stats.ts#L283
    fn per_type_stat(
        traces: &[(ByteString, proto::reports::Trace)],
        field_level_instrumentation_ratio: f64,
    ) -> HashMap<String, SingleTypeStat> {
        fn recur(
            per_type: &mut HashMap<String, SingleTypeStat>,
            field_execution_weight: f64,
            node: &proto::reports::trace::Node,
        ) {
            for child in &node.child {
                recur(per_type, field_execution_weight, child)
            }
            let response_name = if let Some(ResponseName(response_name)) = &node.id {
                response_name
            } else {
                return;
            };
            let field_name = if node.original_field_name.is_empty() {
                response_name
            } else {
                &node.original_field_name
            };
            if field_name.is_empty()
                || node.parent_type.is_empty()
                || node.r#type.is_empty()
                || node.start_time == 0
                || node.end_time == 0
            {
                return;
            }
            let field_stat = per_type
                .entry(node.parent_type.clone())
                .or_default()
                .per_field_stat
                .entry(field_name.clone())
                .or_insert_with(|| metrics::apollo::studio::SingleFieldStat {
                    return_type: node.r#type.clone(), // not `Default::default()`’s empty string
                    errors_count: 0,
                    latency: Default::default(),
                    observed_execution_count: 0,
                    requests_with_errors_count: 0,
                });
            let latency = Duration::from_nanos(node.end_time.saturating_sub(node.start_time));
            field_stat
                .latency
                .increment_duration(Some(latency), field_execution_weight);
            field_stat.observed_execution_count += 1;
            field_stat.errors_count += node.error.len() as u64;
            if !node.error.is_empty() {
                field_stat.requests_with_errors_count += 1;
            }
        }

        // For example, `field_level_instrumentation_ratio == 0.03` means we send a
        // `apollo-federation-include-trace: ftv1` header with 3% of subgraph requests.
        // To compensate, assume that each trace we recieve is representative of 33.3… requests.
        // Metrics that recieve this treatment are kept as floating point values in memory,
        // and converted to integers after aggregating values for a number of requests.
        let field_execution_weight = 1.0 / field_level_instrumentation_ratio;

        let mut per_type = HashMap::new();
        for (_subgraph_name, trace) in traces {
            if let Some(node) = &trace.root {
                recur(&mut per_type, field_execution_weight, node)
            }
        }
        per_type
    }

    fn per_path_error_stats(
        traces: &[(ByteString, proto::reports::Trace)],
    ) -> SinglePathErrorStats {
        fn recur<'node>(
            stats_root: &mut SinglePathErrorStats,
            path: &mut Vec<&'node String>,
            node: &'node proto::reports::trace::Node,
        ) {
            if let Some(ResponseName(name)) = &node.id {
                path.push(name)
            }
            if !node.error.is_empty() {
                let mut stats = &mut *stats_root;
                for &name in &*path {
                    stats = stats.children.entry(name.clone()).or_default();
                }
                stats.errors_count += node.error.len() as u64;
                stats.requests_with_errors_count += 1;
            }
            for child in &node.child {
                recur(stats_root, path, child)
            }
            if let Some(ResponseName(_)) = &node.id {
                path.pop();
            }
        }
        let mut root = Default::default();
        for (subgraph_name, trace) in traces {
            if let Some(node) = &trace.root {
                let path = format!("service:{}", subgraph_name.as_str());
                recur(&mut root, &mut vec![&path], node)
            }
        }
        root
    }

    fn plugin_metrics(config: &Arc<Conf>) {
        let metrics_prom_used = matches!(
            config.metrics,
            Some(Metrics {
                prometheus: Some(_),
                ..
            })
        );
        let metrics_otlp_used = matches!(config.metrics, Some(Metrics { otlp: Some(_), .. }));
        let tracing_otlp_used = matches!(config.tracing, Some(Tracing { otlp: Some(_), .. }));
        let tracing_datadog_used = matches!(
            config.tracing,
            Some(Tracing {
                datadog: Some(_),
                ..
            })
        );
        let tracing_jaeger_used = matches!(
            config.tracing,
            Some(Tracing {
                jaeger: Some(_),
                ..
            })
        );
        let tracing_zipkin_used = matches!(
            config.tracing,
            Some(Tracing {
                zipkin: Some(_),
                ..
            })
        );

        if metrics_prom_used
            || metrics_otlp_used
            || tracing_jaeger_used
            || tracing_otlp_used
            || tracing_zipkin_used
            || tracing_datadog_used
        {
            ::tracing::info!(
                monotonic_counter.apollo.router.operations.telemetry = 1u64,
                telemetry.metrics.otlp = metrics_otlp_used.or_empty(),
                telemetry.metrics.prometheus = metrics_prom_used.or_empty(),
                telemetry.tracing.otlp = tracing_otlp_used.or_empty(),
                telemetry.tracing.datadog = tracing_datadog_used.or_empty(),
                telemetry.tracing.jaeger = tracing_jaeger_used.or_empty(),
                telemetry.tracing.zipkin = tracing_zipkin_used.or_empty(),
            );
        }
    }
}

#[derive(Debug, Clone)]
struct CacheAttributes {
    subgraph_name: Arc<String>,
    headers: http::HeaderMap,
    hashed_query: Arc<String>,
    // Typename + hashed_representation
    representations: Vec<(Arc<String>, Value)>,
}

#[derive(Debug, Hash, Clone)]
struct CacheKey {
    representation: Value,
    typename: Arc<String>,
    query: Arc<String>,
    subgraph_name: Arc<String>,
    hashed_headers: Arc<String>,
}

// Get typename and hashed representation for each representations in the subgraph query
fn extract_cache_attributes(
    representations: &[Value],
) -> Result<Vec<(Arc<String>, Value)>, BoxError> {
    let mut res = Vec::new();
    for representation in representations {
        let opt_type = representation
            .as_object()
            .and_then(|o| o.get(TYPENAME))
            .ok_or("missing __typename in representation")?;
        let typename = opt_type.as_str().unwrap_or("");

        res.push((Arc::new(typename.to_string()), representation.clone()));
    }
    Ok(res)
}

struct CacheCounter {
    primary: Bloom<CacheKey>,
    secondary: Bloom<CacheKey>,
    created_at: Instant,
    ttl: Duration,
}

impl CacheCounter {
    fn new(ttl: Duration) -> Self {
        Self {
            primary: Self::make_filter(),
            secondary: Self::make_filter(),
            created_at: Instant::now(),
            ttl,
        }
    }

    fn make_filter() -> Bloom<CacheKey> {
        // the filter is around 4kB in size (can be calculated with `Bloom::compute_bitmap_size`)
        Bloom::new_for_fp_rate(10000, 0.2)
    }

    fn record(
        &mut self,
        query: Arc<String>,
        subgraph_name: Arc<String>,
        hashed_headers: Arc<String>,
        representations: Vec<(Arc<String>, Value)>,
    ) {
        if self.created_at.elapsed() >= self.ttl {
            self.clear();
        }

        // typename -> (nb of cache hits, nb of entities)
        let mut seen: HashMap<Arc<String>, (usize, usize)> = HashMap::new();
        for (typename, representation) in representations {
            let cache_hit = self.check(&CacheKey {
                representation,
                typename: typename.clone(),
                query: query.clone(),
                subgraph_name: subgraph_name.clone(),
                hashed_headers: hashed_headers.clone(),
            });

            let seen_entry = seen.entry(typename.clone()).or_default();
            if cache_hit {
                seen_entry.0 += 1;
            }
            seen_entry.1 += 1;
        }

        for (typename, (cache_hit, total_entities)) in seen.into_iter() {
            ::tracing::info!(
                histogram.apollo.router.operations.entity.cache_hit = (cache_hit as f64 / total_entities as f64) * 100f64,
                entity_type = %typename,
                subgraph = %subgraph_name,
            );
        }
    }

    fn check(&mut self, key: &CacheKey) -> bool {
        self.primary.check_and_set(key) || self.secondary.check(key)
    }

    fn clear(&mut self) {
        let secondary = std::mem::replace(&mut self.primary, Self::make_filter());
        self.secondary = secondary;

        self.created_at = Instant::now();
    }
}

fn filter_headers(headers: &HeaderMap, forward_rules: &ForwardHeaders) -> String {
    let headers_map = headers
        .iter()
        .filter(|(name, _value)| {
            name != &header::AUTHORIZATION && name != &header::COOKIE && name != &header::SET_COOKIE
        })
        .filter_map(|(name, value)| {
            let send_header = match &forward_rules {
                ForwardHeaders::None => false,
                ForwardHeaders::All => true,
                ForwardHeaders::Only(only) => only.contains(name),
                ForwardHeaders::Except(except) => !except.contains(name),
            };

            send_header.then(|| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or("<unknown>").to_string(),
                )
            })
        })
        .fold(BTreeMap::new(), |mut acc, (name, value)| {
            acc.entry(name).or_insert_with(Vec::new).push(value);
            acc
        });

    match serde_json::to_string(&headers_map) {
        Ok(result) => result,
        Err(_err) => {
            ::tracing::warn!("could not serialize header, trace will not have header information");
            Default::default()
        }
    }
}

// Planner errors return stats report key that start with `## `
// while successful planning stats report key start with `# `
fn licensed_operation_count(stats_report_key: &str) -> u64 {
    if stats_report_key.starts_with("## ") {
        0
    } else {
        1
    }
}

fn convert(
    referenced_fields: router_bridge::planner::ReferencedFieldsForType,
) -> crate::plugins::telemetry::apollo_exporter::proto::reports::ReferencedFieldsForType {
    crate::plugins::telemetry::apollo_exporter::proto::reports::ReferencedFieldsForType {
        field_names: referenced_fields.field_names,
        is_interface: referenced_fields.is_interface,
    }
}

#[derive(Eq, PartialEq, Hash)]
enum ErrorType {
    Trace,
    Metric,
    Other,
}
static OTEL_ERROR_LAST_LOGGED: OnceCell<DashMap<ErrorType, Instant>> = OnceCell::new();

fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    // We have to rate limit these errors because when they happen they are very frequent.
    // Use a dashmap to store the message type with the last time it was logged.
    let last_logged_map = OTEL_ERROR_LAST_LOGGED.get_or_init(DashMap::new);
    let err = err.into();

    // We don't want the dashmap to get big, so we key the error messages by type.
    let error_type = match err {
        opentelemetry::global::Error::Trace(_) => ErrorType::Trace,
        opentelemetry::global::Error::Metric(_) => ErrorType::Metric,
        _ => ErrorType::Other,
    };
    #[cfg(not(test))]
    let threshold = Duration::from_secs(10);
    #[cfg(test)]
    let threshold = Duration::from_millis(100);

    // Copy here so that we don't retain a mutable reference into the dashmap and lock the shard
    let now = Instant::now();
    let last_logged = *last_logged_map
        .entry(error_type)
        .and_modify(|last_logged| {
            if last_logged.elapsed() > threshold {
                *last_logged = now;
            }
        })
        .or_insert_with(|| now);

    if last_logged == now {
        match err {
            opentelemetry::global::Error::Trace(err) => {
                ::tracing::error!("OpenTelemetry trace error occurred: {}", err)
            }
            opentelemetry::global::Error::Metric(err) => {
                ::tracing::error!("OpenTelemetry metric error occurred: {}", err)
            }
            opentelemetry::global::Error::Other(err) => {
                ::tracing::error!("OpenTelemetry error occurred: {}", err)
            }
            other => {
                ::tracing::error!("OpenTelemetry error occurred: {:?}", other)
            }
        }
    }
}

register_plugin!("apollo", "telemetry", Telemetry);

fn request_ftv1(mut req: SubgraphRequest) -> SubgraphRequest {
    if req
        .context
        .private_entries
        .lock()
        .contains_key::<EnableSubgraphFtv1>()
        && Span::current().context().span().span_context().is_sampled()
    {
        req.subgraph_request
            .headers_mut()
            .insert(FTV1_HEADER_NAME.clone(), FTV1_HEADER_VALUE.clone());
    }
    req
}

fn store_ftv1(subgraph_name: &ByteString, resp: SubgraphResponse) -> SubgraphResponse {
    // Stash the FTV1 data
    if resp
        .context
        .private_entries
        .lock()
        .contains_key::<EnableSubgraphFtv1>()
    {
        if let Some(serde_json_bytes::Value::String(ftv1)) =
            resp.response.body().extensions.get("ftv1")
        {
            // Record the ftv1 trace for processing later
            Span::current().record("apollo_private.ftv1", ftv1.as_str());
            resp.context
                .upsert_json_value(SUBGRAPH_FTV1, move |value: Value| {
                    let mut vec = match value {
                        Value::Array(array) => array,
                        // upsert_json_value populate the entry with null if it was vacant
                        Value::Null => Vec::new(),
                        _ => panic!("unexpected JSON value kind"),
                    };
                    vec.push(json!([subgraph_name, ftv1]));
                    Value::Array(vec)
                })
        }
    }
    resp
}

/// CustomTraceIdPropagator to set custom trace_id for our tracing system
/// coming from headers
#[derive(Debug)]
struct CustomTraceIdPropagator {
    header_name: String,
    fields: [String; 1],
}

impl CustomTraceIdPropagator {
    fn new(header_name: String) -> Self {
        Self {
            fields: [header_name.clone()],
            header_name,
        }
    }

    fn extract_span_context(&self, extractor: &dyn Extractor) -> Option<SpanContext> {
        let trace_id = extractor.get(&self.header_name)?;

        // extract trace id
        let trace_id = match opentelemetry::trace::TraceId::from_hex(trace_id) {
            Ok(trace_id) => trace_id,
            Err(err) => {
                ::tracing::error!("cannot generate custom trace_id: {err}");
                return None;
            }
        };

        SpanContext::new(
            trace_id,
            SpanId::INVALID,
            TraceFlags::default().with_sampled(true),
            true,
            TraceState::default(),
        )
        .into()
    }
}

impl TextMapPropagator for CustomTraceIdPropagator {
    fn inject_context(&self, cx: &opentelemetry::Context, injector: &mut dyn Injector) {
        let span = cx.span();
        let span_context = span.span_context();
        if span_context.is_valid() {
            let header_value = format!("{}", span_context.trace_id());
            injector.set(&self.header_name, header_value);
        }
    }

    fn extract_with_context(
        &self,
        cx: &opentelemetry::Context,
        extractor: &dyn Extractor,
    ) -> opentelemetry::Context {
        cx.with_remote_span_context(
            self.extract_span_context(extractor)
                .unwrap_or_else(SpanContext::empty_context),
        )
    }

    fn fields(&self) -> FieldIter<'_> {
        FieldIter::new(self.fields.as_ref())
    }
}

#[derive(Clone)]
struct MetricsAttributes(HashMap<String, AttributeValue>);

#[derive(Clone)]
struct SubgraphMetricsAttributes(HashMap<String, AttributeValue>);

struct EnableSubgraphFtv1;
//
// Please ensure that any tests added to the tests module use the tokio multi-threaded test executor.
//
#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::ops::DerefMut;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use axum::headers::HeaderName;
    use http::HeaderMap;
    use http::HeaderValue;
    use http::StatusCode;
    use insta::assert_snapshot;
    use itertools::Itertools;
    use serde_json::Value;
    use serde_json_bytes::json;
    use serde_json_bytes::ByteString;
    use tower::util::BoxService;
    use tower::Service;
    use tower::ServiceExt;
    use tracing_core::field::Visit;
    use tracing_core::Event;
    use tracing_core::Field;
    use tracing_core::Subscriber;
    use tracing_futures::WithSubscriber;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;

    use super::apollo::ForwardHeaders;
    use crate::error::FetchError;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugin::DynPlugin;
    use crate::plugins::telemetry::handle_error;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::SupergraphRequest;
    use crate::services::SupergraphResponse;

    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_registered() {
        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(
                &serde_json::json!({"apollo": {"schema_id":"abc"}, "tracing": {}}),
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attribute_serialization() {
        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(
                &serde_json::json!({
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
                                "supergraph": {
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
                }),
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics() {
        let mut mock_service = MockSupergraphService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .header("x-custom", "coming_from_header")
                    .data(json!({"data": {"my_value": 2usize}}))
                    .build()
                    .unwrap())
            });

        let mut mock_bad_request_service = MockSupergraphService::new();
        mock_bad_request_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .data(json!({"errors": [{"message": "nope"}]}))
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
                            .extension_code("FETCH_ERROR")
                            .build(),
                    )
                    .build())
            });

        let mut mock_subgraph_service_in_error = MockSubgraphService::new();
        mock_subgraph_service_in_error
            .expect_call()
            .times(1)
            .returning(move |_req: SubgraphRequest| {
                Err(Box::new(FetchError::SubrequestHttpError {
                    status_code: None,
                    service: String::from("my_subgraph_name_error"),
                    reason: String::from("cannot contact the subgraph"),
                }))
            });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
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
                        "service_name": "apollo-router",
                        "attributes": {
                            "supergraph": {
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
                                "all": {
                                    "errors": {
                                        "include_messages": true,
                                        "extensions": [{
                                            "name": "subgraph_error_extended_code",
                                            "path": ".code"
                                        }, {
                                            "name": "message",
                                            "path": ".reason"
                                        }]
                                    }
                                },
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
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap();
        let mut supergraph_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
        let router_req = SupergraphRequest::fake_builder().header("test", "my_value_set");

        let _router_response = supergraph_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        let mut bad_request_supergraph_service =
            dyn_plugin.supergraph_service(BoxService::new(mock_bad_request_service));
        let router_req = SupergraphRequest::fake_builder().header("test", "my_value_set");

        let _router_response = bad_request_supergraph_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        let mut subgraph_service =
            dyn_plugin.subgraph_service("my_subgraph_name", BoxService::new(mock_subgraph_service));
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
        // Another subgraph
        let mut subgraph_service = dyn_plugin.subgraph_service(
            "my_subgraph_name_error",
            BoxService::new(mock_subgraph_service_in_error),
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
            .expect_err("Must be in error");

        let http_req_prom = http::Request::get("http://localhost:9090/WRONG/URL/metrics")
            .body(Default::default())
            .unwrap();
        let mut web_endpoint = dyn_plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();
        let resp = web_endpoint
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let http_req_prom = http::Request::get("http://localhost:9090/metrics")
            .body(Default::default())
            .unwrap();
        let mut resp = web_endpoint.oneshot(http_req_prom).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(resp.body_mut()).await.unwrap();
        let prom_metrics = String::from_utf8_lossy(&body)
            .to_string()
            .split('\n')
            .filter(|l| l.contains("_count") && !l.contains("apollo_router_span_count"))
            .sorted()
            .join("\n");
        assert_snapshot!(prom_metrics);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_custom_buckets() {
        let mut mock_service = MockSupergraphService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .header("x-custom", "coming_from_header")
                    .data(json!({"data": {"my_value": 2usize}}))
                    .build()
                    .unwrap())
            });

        let mut mock_bad_request_service = MockSupergraphService::new();
        mock_bad_request_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .data(json!({"errors": [{"message": "nope"}]}))
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
                            .extension_code("FETCH_ERROR")
                            .build(),
                    )
                    .build())
            });

        let mut mock_subgraph_service_in_error = MockSubgraphService::new();
        mock_subgraph_service_in_error
            .expect_call()
            .times(1)
            .returning(move |_req: SubgraphRequest| {
                Err(Box::new(FetchError::SubrequestHttpError {
                    status_code: None,
                    service: String::from("my_subgraph_name_error"),
                    reason: String::from("cannot contact the subgraph"),
                }))
            });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
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
                        "service_name": "apollo-router",
                        "buckets": [5.0, 10.0, 20.0],
                        "attributes": {
                            "supergraph": {
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
                                "all": {
                                    "errors": {
                                        "include_messages": true,
                                        "extensions": [{
                                            "name": "subgraph_error_extended_code",
                                            "path": ".code"
                                        }, {
                                            "name": "message",
                                            "path": ".reason"
                                        }]
                                    }
                                },
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
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap();
        let mut supergraph_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
        let router_req = SupergraphRequest::fake_builder().header("test", "my_value_set");

        let _router_response = supergraph_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        let mut bad_request_supergraph_service =
            dyn_plugin.supergraph_service(BoxService::new(mock_bad_request_service));
        let router_req = SupergraphRequest::fake_builder().header("test", "my_value_set");

        let _router_response = bad_request_supergraph_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        let mut subgraph_service =
            dyn_plugin.subgraph_service("my_subgraph_name", BoxService::new(mock_subgraph_service));
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
        // Another subgraph
        let mut subgraph_service = dyn_plugin.subgraph_service(
            "my_subgraph_name_error",
            BoxService::new(mock_subgraph_service_in_error),
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
            .expect_err("Must be in error");

        let http_req_prom = http::Request::get("http://localhost:9090/WRONG/URL/metrics")
            .body(Default::default())
            .unwrap();
        let mut web_endpoint = dyn_plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();
        let resp = web_endpoint
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let http_req_prom = http::Request::get("http://localhost:9090/metrics")
            .body(Default::default())
            .unwrap();
        let mut resp = web_endpoint.oneshot(http_req_prom).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(resp.body_mut()).await.unwrap();
        let prom_metrics = String::from_utf8_lossy(&body)
            .to_string()
            .split('\n')
            .filter(|l| l.contains("bucket") && !l.contains("apollo_router_span_count"))
            .sorted()
            .join("\n");
        assert_snapshot!(prom_metrics);
    }

    #[test]
    fn it_test_send_headers_to_studio() {
        let fw_headers = ForwardHeaders::Only(vec![
            HeaderName::from_static("test"),
            HeaderName::from_static("apollo-x-name"),
        ]);
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("xxx"),
        );
        headers.insert(
            HeaderName::from_static("test"),
            HeaderValue::from_static("content"),
        );
        headers.insert(
            HeaderName::from_static("referer"),
            HeaderValue::from_static("test"),
        );
        headers.insert(
            HeaderName::from_static("foo"),
            HeaderValue::from_static("bar"),
        );
        headers.insert(
            HeaderName::from_static("apollo-x-name"),
            HeaderValue::from_static("polaris"),
        );
        let filtered_headers = super::filter_headers(&headers, &fw_headers);
        assert_eq!(
            filtered_headers.as_str(),
            r#"{"apollo-x-name":["polaris"],"test":["content"]}"#
        );
        let filtered_headers = super::filter_headers(&headers, &ForwardHeaders::None);
        assert_eq!(filtered_headers.as_str(), "{}");
    }

    #[tokio::test]
    async fn test_handle_error_throttling() {
        // Set up a fake subscriber so we can check log events. If this is useful then maybe it can be factored out into something reusable
        #[derive(Default)]
        struct TestVisitor {
            log_entries: Vec<String>,
        }

        #[derive(Default, Clone)]
        struct TestLayer {
            visitor: Arc<Mutex<TestVisitor>>,
        }
        impl TestLayer {
            fn assert_log_entry_count(&self, message: &str, expected: usize) {
                let log_entries = self.visitor.lock().unwrap().log_entries.clone();
                let actual = log_entries.iter().filter(|e| e.contains(message)).count();
                assert_eq!(actual, expected);
            }
        }
        impl Visit for TestVisitor {
            fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
                self.log_entries
                    .push(format!("{}={:?}", field.name(), value));
            }
        }

        impl<S> Layer<S> for TestLayer
        where
            S: Subscriber,
            Self: 'static,
        {
            fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
                event.record(self.visitor.lock().unwrap().deref_mut())
            }
        }

        let test_layer = TestLayer::default();

        async {
            // Log twice rapidly, they should get deduped
            handle_error(opentelemetry::global::Error::Other(
                "other error".to_string(),
            ));
            handle_error(opentelemetry::global::Error::Other(
                "other error".to_string(),
            ));
            handle_error(opentelemetry::global::Error::Trace(
                "trace error".to_string().into(),
            ));
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;

        test_layer.assert_log_entry_count("other error", 1);
        test_layer.assert_log_entry_count("trace error", 1);

        // Sleep a bit and then log again, it should get logged
        tokio::time::sleep(Duration::from_millis(200)).await;
        async {
            handle_error(opentelemetry::global::Error::Other(
                "other error".to_string(),
            ));
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;
        test_layer.assert_log_entry_count("other error", 2);
    }
}
