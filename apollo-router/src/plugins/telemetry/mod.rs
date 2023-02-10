//! Telemetry plugin.
// With regards to ELv2 licensing, this entire file is license key functionality
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use ::tracing::field;
use ::tracing::info_span;
use ::tracing::Span;
use axum::headers::HeaderName;
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
use opentelemetry::propagation::text_map_propagator::FieldIter;
use opentelemetry::propagation::Extractor;
use opentelemetry::propagation::Injector;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::propagation::TextMapCompositePropagator;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::trace::TracerProvider;
use opentelemetry::KeyValue;
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
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::fmt::format::JsonFields;
use tracing_subscriber::Layer;

use self::apollo::ForwardValues;
use self::apollo::SingleReport;
use self::apollo_exporter::proto;
use self::apollo_exporter::Sender;
use self::config::Conf;
use self::formatters::text::TextFormatter;
use self::metrics::apollo::studio::SingleTypeStat;
use self::metrics::AttributesForwardConf;
use self::metrics::MetricsAttributesConf;
use self::reload::reload_fmt;
use self::reload::reload_metrics;
use self::reload::OPENTELEMETRY_TRACER_HANDLE;
use self::tracing::reload::ReloadTracer;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::apollo::ForwardHeaders;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::node::Id::ResponseName;
use crate::plugins::telemetry::apollo_exporter::proto::reports::StatsContext;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::Trace;
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
use crate::query_planner::USAGE_REPORTING;
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
// Tracing consts
pub(crate) const SUPERGRAPH_SPAN_NAME: &str = "supergraph";
pub(crate) const SUBGRAPH_SPAN_NAME: &str = "subgraph";
pub(crate) const ROUTER_SPAN_NAME: &str = "router";
pub(crate) const EXECUTION_SPAN_NAME: &str = "execution";
const CLIENT_NAME: &str = "apollo_telemetry::client_name";
const CLIENT_VERSION: &str = "apollo_telemetry::client_version";
const ATTRIBUTES: &str = "apollo_telemetry::metrics_attributes";
const SUBGRAPH_ATTRIBUTES: &str = "apollo_telemetry::subgraph_metrics_attributes";
const SUBGRAPH_FTV1: &str = "apollo_telemetry::subgraph_ftv1";
pub(crate) const STUDIO_EXCLUDE: &str = "apollo_telemetry::studio::exclude";
pub(crate) const LOGGING_DISPLAY_HEADERS: &str = "apollo_telemetry::logging::display_headers";
pub(crate) const LOGGING_DISPLAY_BODY: &str = "apollo_telemetry::logging::display_body";
const DEFAULT_SERVICE_NAME: &str = "apollo-router";
const GLOBAL_TRACER_NAME: &str = "apollo-router";
const DEFAULT_EXPOSE_TRACE_ID_HEADER: &str = "apollo-trace-id";

#[doc(hidden)] // Only public for integration tests
pub struct Telemetry {
    config: Arc<config::Conf>,
    metrics: BasicMetrics,
    // Do not remove _metrics_exporters. Metrics will not be exported if it is removed.
    // Typically the handles are a PushController but may be something else. Dropping the handle will
    // shutdown exporter.
    _metrics_exporters: Vec<MetricsExporterHandle>,
    custom_endpoints: MultiMap<ListenAddr, Endpoint>,
    apollo_metrics_sender: apollo_exporter::Sender,
    field_level_instrumentation_ratio: f64,

    tracer_provider: Option<opentelemetry::sdk::trace::TracerProvider>,
    meter_provider: AggregateMeterProvider,
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
        // If for some reason we didn't use the trace provider then safely discard it e.g. some other plugin failed `new`
        // To ensure we don't hang tracing providers are dropped in a blocking task.
        // https://github.com/open-telemetry/opentelemetry-rust/issues/868#issuecomment-1250387989
        // We don't have to worry about timeouts as every exporter is batched, which has a timeout on it already.
        if let Some(tracer_provider) = self.tracer_provider.take() {
            // If we have no runtime then we don't need to spawn a task as we are already in a blocking context.
            if Handle::try_current().is_ok() {
                tokio::task::spawn_blocking(move || drop(tracer_provider));
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
        Ok(Telemetry {
            custom_endpoints: metrics_builder.custom_endpoints(),
            _metrics_exporters: metrics_builder.exporters(),
            metrics: BasicMetrics::new(&meter_provider),
            apollo_metrics_sender: metrics_builder.apollo_metrics_provider(),
            field_level_instrumentation_ratio,
            tracer_provider: Some(Self::create_tracer_provider(&config)?),
            meter_provider,
            config: Arc::new(config),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let config = self.config.clone();
        let config_later = self.config.clone();

        ServiceBuilder::new()
            .instrument(move |request: &router::Request| {
                let apollo = config.apollo.as_ref().cloned().unwrap_or_default();
                let trace_id = TraceId::maybe_new()
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                let router_request = &request.router_request;
                let headers = router_request.headers();
                let client_name = headers
                    .get(&apollo.client_name_header)
                    .cloned()
                    .unwrap_or_else(|| HeaderValue::from_static(""));
                let client_version = headers
                    .get(&apollo.client_version_header)
                    .cloned()
                    .unwrap_or_else(|| HeaderValue::from_static(""));
                let span = ::tracing::info_span!(ROUTER_SPAN_NAME,
                    "http.method" = %router_request.method(),
                    "http.route" = %router_request.uri(),
                    "http.flavor" = ?router_request.version(),
                    "trace_id" = %trace_id,
                    "client.name" = client_name.to_str().unwrap_or_default(),
                    "client.version" = client_version.to_str().unwrap_or_default(),
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
                async move {
                    let span = Span::current();
                    let response: Result<router::Response, BoxError> = fut.await;

                    span.record(
                        "apollo_private.duration_ns",
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
                if let Ok(Some(usage_reporting)) =
                    resp.context.get::<_, UsageReporting>(USAGE_REPORTING)
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
                            .unwrap_or(HeaderName::from_static(DEFAULT_EXPOSE_TRACE_ID_HEADER))
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
                    Self::populate_context(config.clone(), req);
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
                        Self::update_metrics_on_last_response(
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
            .instrument(move |_req: &ExecutionRequest| {
                info_span!("execution", "otel.kind" = "INTERNAL",)
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
        let name = name.to_owned();
        let apollo_handler = self.apollo_handler();
        ServiceBuilder::new()
            .instrument(move |req: &SubgraphRequest| {
                let query = req
                    .subgraph_request
                    .body()
                    .query
                    .clone()
                    .unwrap_or_default();
                let operation_name = req
                    .subgraph_request
                    .body()
                    .operation_name
                    .clone()
                    .unwrap_or_default();

                info_span!(
                    SUBGRAPH_SPAN_NAME,
                    "apollo.subgraph.name" = name.as_str(),
                    graphql.document = query.as_str(),
                    graphql.operation.name = operation_name.as_str(),
                    "otel.kind" = "INTERNAL",
                    "apollo_private.ftv1" = field::Empty
                )
            })
            .map_request(move |req| apollo_handler.request_ftv1(req))
            .map_response(move |resp| apollo_handler.store_ftv1(&subgraph_name, resp))
            .map_future_with_request_data(
                move |sub_request: &SubgraphRequest| {
                    Self::store_subgraph_request_attributes(
                        subgraph_metrics_conf_req.clone(),
                        sub_request,
                    );
                    sub_request.context.clone()
                },
                move |context: Context,
                      f: BoxFuture<'static, Result<SubgraphResponse, BoxError>>| {
                    let metrics = metrics.clone();
                    let subgraph_attribute = subgraph_attribute.clone();
                    let subgraph_metrics_conf = subgraph_metrics_conf_resp.clone();
                    // Using Instant because it is guaranteed to be monotonically increasing.
                    let now = Instant::now();
                    f.map(move |result: Result<SubgraphResponse, BoxError>| {
                        Self::store_subgraph_response_attributes(
                            &context,
                            metrics,
                            subgraph_attribute,
                            subgraph_metrics_conf,
                            now,
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
        if let Some(from_request_header) = &propagation.request.header_name {
            propagators.push(Box::new(CustomTraceIdPropagator::new(
                from_request_header.to_string(),
            )));
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
        // For metrics
        builder = builder.with_simple_exporter(metrics::span_metrics_exporter::Exporter::default());

        let tracer_provider = builder.build();
        Ok(tracer_provider)
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

    #[allow(clippy::type_complexity)]
    fn create_fmt_layer(
        config: &config::Conf,
    ) -> Box<
        dyn Layer<
                ::tracing_subscriber::layer::Layered<
                    OpenTelemetryLayer<
                        ::tracing_subscriber::Registry,
                        ReloadTracer<::opentelemetry::sdk::trace::Tracer>,
                    >,
                    ::tracing_subscriber::Registry,
                >,
            > + Send
            + Sync,
    > {
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
            let query = http_request.body().query.clone().unwrap_or_default();
            let operation_name = http_request
                .body()
                .operation_name
                .clone()
                .unwrap_or_default();

            let span = info_span!(
                SUPERGRAPH_SPAN_NAME,
                graphql.document = query.as_str(),
                // TODO add graphql.operation.type
                graphql.operation.name = operation_name.as_str(),
                otel.kind = "INTERNAL",
                apollo_private.field_level_instrumentation_ratio =
                    field_level_instrumentation_ratio,
                apollo_private.operation_signature = field::Empty,
                apollo_private.graphql.variables = Self::filter_variables_values(
                    &request.supergraph_request.body().variables,
                    &config.send_variable_values,
                ),
            );

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

    fn populate_context(config: Arc<Conf>, req: &SupergraphRequest) {
        let apollo_config = config.apollo.clone().unwrap_or_default();
        let context = &req.context;
        let http_request = &req.supergraph_request;
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
            let mut attributes: HashMap<String, String> = HashMap::new();
            if let Some(operation_name) = &req.supergraph_request.body().operation_name {
                attributes.insert("operation_name".to_string(), operation_name.clone());
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

            let _ = context.insert(ATTRIBUTES, attributes);
        }
    }

    fn apollo_handler(&self) -> ApolloFtv1Handler {
        let mut rng = rand::thread_rng();

        if rng.gen_bool(self.field_level_instrumentation_ratio) {
            ApolloFtv1Handler::Enabled
        } else {
            ApolloFtv1Handler::Disabled
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
            .insert(SUBGRAPH_ATTRIBUTES, attributes)
            .unwrap();
    }

    fn store_subgraph_response_attributes(
        context: &Context,
        metrics: BasicMetrics,
        subgraph_attribute: KeyValue,
        attribute_forward_config: Arc<Option<AttributesForwardConf>>,
        now: Instant,
        result: &Result<Response, BoxError>,
    ) {
        let mut metric_attrs = context
            .get::<_, HashMap<String, String>>(SUBGRAPH_ATTRIBUTES)
            .ok()
            .flatten()
            .map(|attrs| {
                attrs
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
    fn update_metrics_on_last_response(
        ctx: &Context,
        config: Arc<Conf>,
        field_level_instrumentation_ratio: f64,
        metrics: BasicMetrics,
        sender: Sender,
        start: Instant,
        result: Result<supergraph::Response, BoxError>,
    ) -> Result<supergraph::Response, BoxError> {
        match result {
            Err(e) => {
                if !matches!(sender, Sender::Noop) {
                    Self::update_apollo_metrics(
                        ctx,
                        field_level_instrumentation_ratio,
                        sender,
                        true,
                        start.elapsed(),
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
                let mut has_errors = !router_response.response.status().is_success();
                Ok(router_response.map(move |response_stream| {
                    let sender = sender.clone();
                    let ctx = ctx.clone();

                    response_stream
                        .map(move |response| {
                            if !response.errors.is_empty() {
                                has_errors = true;
                            }

                            if !response.has_next.unwrap_or(false)
                                && !matches!(sender, Sender::Noop)
                            {
                                Self::update_apollo_metrics(
                                    &ctx,
                                    field_level_instrumentation_ratio,
                                    sender.clone(),
                                    has_errors,
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

    fn update_apollo_metrics(
        context: &Context,
        field_level_instrumentation_ratio: f64,
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
                SingleStatsReport {
                    operation_count,
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
                    operation_count,
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
                operation_count: 1,
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
fn operation_count(stats_report_key: &str) -> u64 {
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

fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    match err.into() {
        opentelemetry::global::Error::Trace(err) => {
            ::tracing::error!("OpenTelemetry trace error occurred: {}", err)
        }
        opentelemetry::global::Error::Metric(err_msg) => {
            ::tracing::error!("OpenTelemetry metric error occurred: {}", err_msg)
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

/// This enum is a partial cleanup of the telemetry plugin logic.
///
#[derive(Copy, Clone)]
enum ApolloFtv1Handler {
    Enabled,
    Disabled,
}

impl ApolloFtv1Handler {
    fn request_ftv1(&self, mut req: SubgraphRequest) -> SubgraphRequest {
        if let ApolloFtv1Handler::Enabled = self {
            if Span::current().context().span().span_context().is_sampled() {
                req.subgraph_request.headers_mut().insert(
                    "apollo-federation-include-trace",
                    HeaderValue::from_static("ftv1"),
                );
            }
        }
        req
    }

    fn store_ftv1(&self, subgraph_name: &ByteString, resp: SubgraphResponse) -> SubgraphResponse {
        // Stash the FTV1 data
        if let ApolloFtv1Handler::Enabled = self {
            if let Some(serde_json_bytes::Value::String(ftv1)) =
                resp.response.body().extensions.get("ftv1")
            {
                // Record the ftv1 trace for processing later
                Span::current().record("apollo_private.ftv1", ftv1.as_str());
                resp.context
                    .upsert_json_value(SUBGRAPH_FTV1, |value: Value| {
                        let mut vec = match value {
                            Value::Array(array) => array,
                            Value::Null => Vec::new(),
                            _ => panic!("unexpected JSON value kind"),
                        };
                        vec.push(json!([subgraph_name.clone(), ftv1.clone()]));
                        Value::Array(vec)
                    })
            }
        }
        resp
    }
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

//
// Please ensure that any tests added to the tests module use the tokio multi-threaded test executor.
//
#[cfg(test)]
mod tests {
    use std::str::FromStr;

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

    use super::apollo::ForwardHeaders;
    use crate::error::FetchError;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugin::DynPlugin;
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
}
