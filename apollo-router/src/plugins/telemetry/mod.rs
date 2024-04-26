//! Telemetry plugin.
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use ::tracing::info_span;
use ::tracing::Span;
use axum::headers::HeaderName;
use config_new::Selectors;
use dashmap::DashMap;
use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::StreamExt;
use http::header;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use multimap::MultiMap;
use once_cell::sync::OnceCell;
use opentelemetry::global::GlobalTracerProvider;
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
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
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

use self::apollo::ForwardValues;
use self::apollo::LicensedOperationCountByType;
use self::apollo::OperationSubType;
use self::apollo::SingleReport;
use self::apollo_exporter::proto;
use self::apollo_exporter::Sender;
use self::config::Conf;
use self::config::Sampler;
use self::config::SamplerOption;
use self::config::TraceIdFormat;
use self::config_new::events::RouterEvents;
use self::config_new::events::SubgraphEvents;
use self::config_new::events::SupergraphEvents;
use self::config_new::instruments::Instrumented;
use self::config_new::instruments::RouterInstruments;
use self::config_new::instruments::SubgraphInstruments;
use self::config_new::instruments::SupergraphCustomInstruments;
use self::config_new::spans::Spans;
use self::metrics::apollo::studio::SingleTypeStat;
use self::metrics::AttributesForwardConf;
use self::reload::reload_fmt;
use self::reload::SamplingFilter;
pub(crate) use self::span_factory::SpanMode;
use self::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use self::tracing::apollo_telemetry::CLIENT_NAME_KEY;
use self::tracing::apollo_telemetry::CLIENT_VERSION_KEY;
use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::layers::instrument::InstrumentLayer;
use crate::layers::ServiceBuilderExt;
use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::metrics::meter_provider;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::apollo::ForwardHeaders;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::node::Id::ResponseName;
use crate::plugins::telemetry::apollo_exporter::proto::reports::StatsContext;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::fmt_layer::create_fmt_layer;
use crate::plugins::telemetry::metrics::apollo::studio::SingleContextualizedStats;
use crate::plugins::telemetry::metrics::apollo::studio::SinglePathErrorStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleQueryLatencyStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStatsReport;
use crate::plugins::telemetry::metrics::prometheus::commit_prometheus;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::reload::metrics_layer;
use crate::plugins::telemetry::reload::OPENTELEMETRY_TRACER_HANDLE;
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
use crate::tracer::TraceId;
use crate::Context;
use crate::ListenAddr;

pub(crate) mod apollo;
pub(crate) mod apollo_exporter;
pub(crate) mod config;
pub(crate) mod config_new;
pub(crate) mod dynamic_attribute;
mod endpoint;
mod fmt_layer;
pub(crate) mod formatters;
mod logging;
pub(crate) mod metrics;
/// Opentelemetry utils
pub(crate) mod otel;
mod otlp;
pub(crate) mod reload;
mod resource;
mod span_factory;
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
pub(crate) const STUDIO_EXCLUDE: &str = "apollo_telemetry::studio::exclude";
pub(crate) const LOGGING_DISPLAY_HEADERS: &str = "apollo_telemetry::logging::display_headers";
pub(crate) const LOGGING_DISPLAY_BODY: &str = "apollo_telemetry::logging::display_body";

pub(crate) const OTEL_STATUS_CODE: &str = "otel.status_code";
#[allow(dead_code)]
pub(crate) const OTEL_STATUS_DESCRIPTION: &str = "otel.status_description";
pub(crate) const OTEL_STATUS_CODE_OK: &str = "OK";
pub(crate) const OTEL_STATUS_CODE_ERROR: &str = "ERROR";
const GLOBAL_TRACER_NAME: &str = "apollo-router";
const DEFAULT_EXPOSE_TRACE_ID_HEADER: &str = "apollo-trace-id";
static DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME: HeaderName =
    HeaderName::from_static(DEFAULT_EXPOSE_TRACE_ID_HEADER);
static FTV1_HEADER_NAME: HeaderName = HeaderName::from_static("apollo-federation-include-trace");
static FTV1_HEADER_VALUE: HeaderValue = HeaderValue::from_static("ftv1");

#[doc(hidden)] // Only public for integration tests
pub(crate) struct Telemetry {
    config: Arc<config::Conf>,
    custom_endpoints: MultiMap<ListenAddr, Endpoint>,
    apollo_metrics_sender: apollo_exporter::Sender,
    field_level_instrumentation_ratio: f64,
    sampling_filter_ratio: SamplerOption,

    activation: Mutex<TelemetryActivation>,
}

struct TelemetryActivation {
    tracer_provider: Option<opentelemetry::sdk::trace::TracerProvider>,
    // We have to have separate meter providers for prometheus metrics so that they don't get zapped on router reload.
    public_meter_provider: Option<FilterMeterProvider>,
    public_prometheus_meter_provider: Option<FilterMeterProvider>,
    private_meter_provider: Option<FilterMeterProvider>,
    is_active: bool,
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
    configurator: &T,
    tracing_config: &TracingCommon,
    spans_config: &Spans,
) -> Result<Builder, BoxError> {
    if configurator.enabled() {
        builder = configurator.apply(builder, tracing_config, spans_config)?;
    }
    Ok(builder)
}

fn setup_metrics_exporter<T: MetricsConfigurator>(
    mut builder: MetricsBuilder,
    configurator: &T,
    metrics_common: &MetricsCommon,
) -> Result<MetricsBuilder, BoxError> {
    if configurator.enabled() {
        builder = configurator.apply(builder, metrics_common)?;
    }
    Ok(builder)
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        let mut activation = self.activation.lock();
        let metrics_providers: [Option<FilterMeterProvider>; 3] = [
            activation.private_meter_provider.take(),
            activation.public_meter_provider.take(),
            activation.public_prometheus_meter_provider.take(),
        ];
        let tracer_provider = activation.tracer_provider.take();
        drop(activation);
        TelemetryActivation::checked_meter_shutdown(metrics_providers);

        if let Some(tracer_provider) = tracer_provider {
            Self::checked_tracer_shutdown(tracer_provider);
        }
    }
}

#[async_trait::async_trait]
impl Plugin for Telemetry {
    type Config = config::Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        opentelemetry::global::set_error_handler(handle_error)
            .expect("otel error handler lock poisoned, fatal");

        let mut config = init.config;
        config.instrumentation.spans.update_defaults();
        config.instrumentation.instruments.update_defaults();
        config.exporters.logging.validate()?;

        let field_level_instrumentation_ratio =
            config.calculate_field_level_instrumentation_ratio()?;
        let metrics_builder = Self::create_metrics_builder(&config)?;

        let (sampling_filter_ratio, tracer_provider) = Self::create_tracer_provider(&config)?;

        if config.instrumentation.spans.mode == SpanMode::Deprecated {
            ::tracing::warn!("telemetry.instrumentation.spans.mode is currently set to 'deprecated', either explicitly or via defaulting. Set telemetry.instrumentation.spans.mode explicitly in your router.yaml to 'spec_compliant' for log and span attributes that follow OpenTelemetry semantic conventions. This option will be defaulted to 'spec_compliant' in a future release and eventually removed altogether");
        }

        Ok(Telemetry {
            custom_endpoints: metrics_builder.custom_endpoints,
            apollo_metrics_sender: metrics_builder.apollo_metrics_sender,
            field_level_instrumentation_ratio,
            activation: Mutex::new(TelemetryActivation {
                tracer_provider: Some(tracer_provider),
                public_meter_provider: Some(FilterMeterProvider::public(
                    metrics_builder.public_meter_provider_builder.build(),
                )),
                private_meter_provider: Some(FilterMeterProvider::private(
                    metrics_builder.apollo_meter_provider_builder.build(),
                )),
                public_prometheus_meter_provider: metrics_builder
                    .prometheus_meter_provider
                    .map(FilterMeterProvider::public),
                is_active: false,
            }),
            sampling_filter_ratio,
            config: Arc::new(config),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let config = self.config.clone();
        let config_later = self.config.clone();
        let config_request = self.config.clone();
        let span_mode = config.instrumentation.spans.mode;
        let use_legacy_request_span =
            matches!(config.instrumentation.spans.mode, SpanMode::Deprecated);
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        let metrics_sender = self.apollo_metrics_sender.clone();

        ServiceBuilder::new()
            .map_response(move |response: router::Response| {
                // The current span *should* be the request span as we are outside the instrument block.
                let span = Span::current();
                if let Some(span_name) = span.metadata().map(|metadata| metadata.name()) {
                    if (use_legacy_request_span && span_name == REQUEST_SPAN_NAME)
                        || (!use_legacy_request_span && span_name == ROUTER_SPAN_NAME)
                    {
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
                            (Ok(Some(kind)), Ok(Some(name))) => {
                                span.record("otel.name", format!("{kind} {name}"))
                            }
                            (Ok(Some(kind)), _) => span.record("otel.name", kind),
                            _ => span.record("otel.name", "GraphQL Operation"),
                        };
                    }
                }

                response
            })
            .option_layer(use_legacy_request_span.then(move || {
                InstrumentLayer::new(move |request: &router::Request| {
                    span_mode.create_router(&request.router_request)
                })
            }))
            .map_future_with_request_data(
                move |request: &router::Request| {
                    if !use_legacy_request_span {
                        let span = Span::current();

                        span.set_span_dyn_attribute(
                            HTTP_REQUEST_METHOD,
                            request.router_request.method().to_string().into(),
                        );
                    }

                    let client_name = request
                        .router_request
                        .headers()
                        .get(&config_request.apollo.client_name_header)
                        .and_then(|h| h.to_str().ok());
                    let client_version = request
                        .router_request
                        .headers()
                        .get(&config_request.apollo.client_version_header)
                        .and_then(|h| h.to_str().ok());

                    if let Some(name) = client_name {
                        let _ = request.context.insert(CLIENT_NAME, name.to_owned());
                    }

                    if let Some(version) = client_version {
                        let _ = request.context.insert(CLIENT_VERSION, version.to_owned());
                    }

                    let mut custom_attributes = config_request
                        .instrumentation
                        .spans
                        .router
                        .attributes
                        .on_request(request);

                    custom_attributes.extend([
                        KeyValue::new(CLIENT_NAME_KEY, client_name.unwrap_or("").to_string()),
                        KeyValue::new(CLIENT_VERSION_KEY, client_version.unwrap_or("").to_string()),
                        KeyValue::new(
                            Key::from_static_str("apollo_private.http.request_headers"),
                            filter_headers(
                                request.router_request.headers(),
                                &config_request.apollo.send_headers,
                            ),
                        ),
                    ]);

                    let custom_instruments: RouterInstruments = config_request
                        .instrumentation
                        .instruments
                        .new_router_instruments();
                    custom_instruments.on_request(request);

                    let custom_events: RouterEvents =
                        config_request.instrumentation.events.new_router_events();
                    custom_events.on_request(request);

                    (
                        custom_attributes,
                        custom_instruments,
                        custom_events,
                        request.context.clone(),
                    )
                },
                move |(custom_attributes, custom_instruments, custom_events, ctx): (
                    Vec<KeyValue>,
                    RouterInstruments,
                    RouterEvents,
                    Context,
                ),
                      fut| {
                    let start = Instant::now();
                    let config = config_later.clone();
                    let sender = metrics_sender.clone();

                    Self::plugin_metrics(&config);

                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_attributes);
                        let response: Result<router::Response, BoxError> = fut.await;

                        span.record(
                            APOLLO_PRIVATE_DURATION_NS,
                            start.elapsed().as_nanos() as i64,
                        );

                        let expose_trace_id = &config.exporters.tracing.response_trace_id;
                        if let Ok(response) = &response {
                            span.set_span_dyn_attributes(
                                config
                                    .instrumentation
                                    .spans
                                    .router
                                    .attributes
                                    .on_response(response),
                            );
                            custom_instruments.on_response(response);
                            custom_events.on_response(response);

                            if expose_trace_id.enabled {
                                let header_name = expose_trace_id
                                    .header_name
                                    .as_ref()
                                    .unwrap_or(&DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME);
                                let mut headers: HashMap<String, Vec<String>> =
                                    HashMap::with_capacity(1);
                                if let Some(value) = response.response.headers().get(header_name) {
                                    headers.insert(
                                        header_name.to_string(),
                                        vec![value.to_str().unwrap_or_default().to_string()],
                                    );
                                    let response_headers =
                                        serde_json::to_string(&headers).unwrap_or_default();
                                    span.record(
                                        "apollo_private.http.response_headers",
                                        &response_headers,
                                    );
                                }
                            }

                            if response
                                .context
                                .extensions()
                                .lock()
                                .get::<Arc<UsageReporting>>()
                                .map(|u| {
                                    u.stats_report_key == "## GraphQLValidationFailure\n"
                                        || u.stats_report_key == "## GraphQLParseFailure\n"
                                })
                                .unwrap_or(false)
                            {
                                Self::update_apollo_metrics(
                                    &response.context,
                                    field_level_instrumentation_ratio,
                                    sender,
                                    true,
                                    start.elapsed(),
                                    // the query is invalid, we did not parse the operation kind
                                    OperationKind::Query,
                                    None,
                                );
                            }

                            if response.response.status() >= StatusCode::BAD_REQUEST {
                                span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            } else {
                                span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
                            }
                        } else if let Err(err) = &response {
                            span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            span.set_span_dyn_attributes(
                                config.instrumentation.spans.router.attributes.on_error(err),
                            );
                            custom_instruments.on_error(err, &ctx);
                            custom_events.on_error(err, &ctx);
                        }

                        response
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let metrics_sender = self.apollo_metrics_sender.clone();
        let span_mode = self.config.instrumentation.spans.mode;
        let config = self.config.clone();
        let config_instrument = self.config.clone();
        let config_map_res_first = config.clone();
        let config_map_res = config.clone();
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        ServiceBuilder::new()
            .instrument(move |supergraph_req: &SupergraphRequest| span_mode.create_supergraph(
                &config_instrument.apollo,
                supergraph_req,
                field_level_instrumentation_ratio,
            ))
            .map_response(move |mut resp: SupergraphResponse| {
                let config = config_map_res_first.clone();
                if let Some(usage_reporting) = {
                    let extensions = resp.context.extensions().lock();
                    let urp = extensions.get::<Arc<UsageReporting>>();
                    urp.cloned()
                }
                {
                    // Record the operation signature on the router span
                    Span::current().record(
                        APOLLO_PRIVATE_OPERATION_SIGNATURE.as_str(),
                        usage_reporting.stats_report_key.as_str(),
                    );
                }
                // To expose trace_id or not
                let expose_trace_id_header = config.exporters.tracing.response_trace_id.enabled.then(|| {
                    config.exporters.tracing.response_trace_id
                        .header_name
                        .clone()
                        .unwrap_or_else(|| DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME.clone())
                });

                // Append the trace ID with the right format, based on the config
                let format_id = |trace: TraceId| {
                    let id = match config.exporters.tracing.response_trace_id.format {
                        TraceIdFormat::Hexadecimal => format!("{:032x}", trace.to_u128()),
                        TraceIdFormat::Decimal => format!("{}", trace.to_u128()),
                    };

                    HeaderValue::from_str(&id).ok()
                };
                if let (Some(header_name), Some(trace_id)) = (
                    expose_trace_id_header,
                    TraceId::maybe_new().and_then(format_id),
                ) {
                    resp.response.headers_mut().append(header_name, trace_id);
                }

                if resp.context.contains_key(LOGGING_DISPLAY_HEADERS) {
                    let sorted_headers = resp
                        .response
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.as_str(), v))
                        .collect::<BTreeMap<_, _>>();
                    ::tracing::info!(http.response.headers = ?sorted_headers, "Supergraph response headers");
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
                    let custom_attributes = config.instrumentation.spans.supergraph.attributes.on_request(req);
                    Self::populate_context(config.clone(), field_level_instrumentation_ratio, req);
                    let custom_instruments = SupergraphCustomInstruments::new(
                        &config.instrumentation.instruments.supergraph.custom,
                    );
                    custom_instruments.on_request(req);

                    let supergraph_events = config.instrumentation.events.new_supergraph_events();
                    supergraph_events.on_request(req);

                    (req.context.clone(), custom_instruments, custom_attributes, supergraph_events)
                },
                move |(ctx, custom_instruments, custom_attributes, supergraph_events): (Context, SupergraphCustomInstruments, Vec<KeyValue>, SupergraphEvents), fut| {
                    let config = config_map_res.clone();
                    let sender = metrics_sender.clone();
                    let start = Instant::now();

                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_attributes);
                        let mut result: Result<SupergraphResponse, BoxError> = fut.await;
                        match &result {
                            Ok(resp) => {
                                span.set_span_dyn_attributes(config.instrumentation.spans.supergraph.attributes.on_response(resp));
                                custom_instruments.on_response(resp);
                                supergraph_events.on_response(resp);
                            },
                            Err(err) => {
                                span.set_span_dyn_attributes(config.instrumentation.spans.supergraph.attributes.on_error(err));
                                custom_instruments.on_error(err, &ctx);
                                supergraph_events.on_error(err, &ctx);
                            },
                        }
                        result = Self::update_otel_metrics(
                            config.clone(),
                            ctx.clone(),
                            result,
                            start.elapsed(),
                        )
                        .await;
                        Self::update_metrics_on_response_events(
                            &ctx, config, field_level_instrumentation_ratio, sender, start, result,
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
        let config = self.config.clone();
        let span_mode = self.config.instrumentation.spans.mode;
        let conf = self.config.clone();
        let subgraph_attribute = KeyValue::new("subgraph", name.to_string());
        let subgraph_metrics_conf_req = self.create_subgraph_metrics_conf(name);
        let subgraph_metrics_conf_resp = subgraph_metrics_conf_req.clone();
        let subgraph_name = ByteString::from(name);
        let name = name.to_owned();
        ServiceBuilder::new()
            .instrument(move |req: &SubgraphRequest| span_mode.create_subgraph(name.as_str(), req))
            .map_request(move |req: SubgraphRequest| request_ftv1(req))
            .map_response(move |resp| store_ftv1(&subgraph_name, resp))
            .map_future_with_request_data(
                move |sub_request: &SubgraphRequest| {
                    Self::store_subgraph_request_attributes(
                        subgraph_metrics_conf_req.as_ref(),
                        sub_request,
                    );

                    let custom_attributes = config
                        .instrumentation
                        .spans
                        .subgraph
                        .attributes
                        .on_request(sub_request);
                    let custom_instruments = config
                        .instrumentation
                        .instruments
                        .new_subgraph_instruments();
                    custom_instruments.on_request(sub_request);
                    let custom_events = config.instrumentation.events.new_subgraph_events();
                    custom_events.on_request(sub_request);

                    (
                        sub_request.context.clone(),
                        custom_instruments,
                        custom_attributes,
                        custom_events,
                    )
                },
                move |(context, custom_instruments, custom_attributes, custom_events): (
                    Context,
                    SubgraphInstruments,
                    Vec<KeyValue>,
                    SubgraphEvents,
                ),
                      f: BoxFuture<'static, Result<SubgraphResponse, BoxError>>| {
                    let subgraph_attribute = subgraph_attribute.clone();
                    let subgraph_metrics_conf = subgraph_metrics_conf_resp.clone();
                    let conf = conf.clone();
                    // Using Instant because it is guaranteed to be monotonically increasing.
                    let now = Instant::now();
                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_attributes);
                        let result: Result<SubgraphResponse, BoxError> = f.await;

                        match &result {
                            Ok(resp) => {
                                if resp.response.status() >= StatusCode::BAD_REQUEST {
                                    span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                } else {
                                    span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
                                }
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .subgraph
                                        .attributes
                                        .on_response(resp),
                                );
                                custom_instruments.on_response(resp);
                                custom_events.on_response(resp);
                            }
                            Err(err) => {
                                span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);

                                span.set_span_dyn_attributes(
                                    conf.instrumentation.spans.subgraph.attributes.on_error(err),
                                );
                                custom_instruments.on_error(err, &context);
                                custom_events.on_error(err, &context);
                            }
                        }

                        Self::store_subgraph_response_attributes(
                            &context,
                            subgraph_attribute,
                            subgraph_metrics_conf.as_ref(),
                            now,
                            &result,
                        );
                        result
                    }
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
    pub(crate) fn activate(&self) {
        let mut activation = self.activation.lock();
        if activation.is_active {
            return;
        }

        // Only apply things if we were executing in the context of a vanilla the Apollo executable.
        // Users that are rolling their own routers will need to set up telemetry themselves.
        if let Some(hot_tracer) = OPENTELEMETRY_TRACER_HANDLE.get() {
            SamplingFilter::configure(&self.sampling_filter_ratio);

            // The reason that this has to happen here is that we are interacting with global state.
            // If we do this logic during plugin init then if a subsequent plugin fails to init then we
            // will already have set the new tracer provider and we will be in an inconsistent state.
            // activate is infallible, so if we get here we know the new pipeline is ready to go.
            let tracer_provider = activation
                .tracer_provider
                .take()
                .expect("must have new tracer_provider");

            let tracer = tracer_provider.versioned_tracer(
                GLOBAL_TRACER_NAME,
                Some(env!("CARGO_PKG_VERSION")),
                None::<String>,
                None,
            );
            hot_tracer.reload(tracer);

            let last_provider = opentelemetry::global::set_tracer_provider(tracer_provider);

            Self::checked_global_tracer_shutdown(last_provider);

            opentelemetry::global::set_text_map_propagator(Self::create_propagator(&self.config));
        }

        activation.reload_metrics();

        reload_fmt(create_fmt_layer(&self.config));
        activation.is_active = true;
    }

    fn create_propagator(config: &config::Conf) -> TextMapCompositePropagator {
        let propagation = &config.exporters.tracing.propagation;

        let tracing = &config.exporters.tracing;

        let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
        // TLDR the jaeger propagator MUST BE the first one because the version of opentelemetry_jaeger is buggy.
        // It overrides the current span context with an empty one if it doesn't find the corresponding headers.
        // Waiting for the >=0.16.1 release
        if propagation.jaeger || tracing.jaeger.enabled() {
            propagators.push(Box::<opentelemetry_jaeger::Propagator>::default());
        }
        if propagation.baggage {
            propagators.push(Box::<opentelemetry::sdk::propagation::BaggagePropagator>::default());
        }
        if propagation.trace_context || tracing.otlp.enabled {
            propagators
                .push(Box::<opentelemetry::sdk::propagation::TraceContextPropagator>::default());
        }
        if propagation.zipkin || tracing.zipkin.enabled {
            propagators.push(Box::<opentelemetry_zipkin::Propagator>::default());
        }
        if propagation.datadog || tracing.datadog.enabled {
            propagators.push(Box::<opentelemetry_datadog::DatadogPropagator>::default());
        }
        if propagation.aws_xray {
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
        let tracing_config = &config.exporters.tracing;
        let spans_config = &config.instrumentation.spans;
        let mut common = tracing_config.common.clone();
        let mut sampler = common.sampler.clone();
        // set it to AlwaysOn: it is now done in the SamplingFilter, so whatever is sent to an exporter
        // should be accepted
        common.sampler = SamplerOption::Always(Sampler::AlwaysOn);

        let mut builder =
            opentelemetry::sdk::trace::TracerProvider::builder().with_config((&common).into());

        builder = setup_tracing(builder, &tracing_config.jaeger, &common, spans_config)?;
        builder = setup_tracing(builder, &tracing_config.zipkin, &common, spans_config)?;
        builder = setup_tracing(builder, &tracing_config.datadog, &common, spans_config)?;
        builder = setup_tracing(builder, &tracing_config.otlp, &common, spans_config)?;
        builder = setup_tracing(builder, &config.apollo, &common, spans_config)?;

        if !tracing_config.jaeger.enabled()
            && !tracing_config.zipkin.enabled()
            && !tracing_config.datadog.enabled()
            && !TracingConfigurator::enabled(&tracing_config.otlp)
            && !TracingConfigurator::enabled(&config.apollo)
        {
            sampler = SamplerOption::Always(Sampler::AlwaysOff);
        }

        let tracer_provider = builder.build();
        Ok((sampler, tracer_provider))
    }

    fn create_metrics_builder(config: &config::Conf) -> Result<MetricsBuilder, BoxError> {
        let metrics_config = &config.exporters.metrics;
        let metrics_common_config = &metrics_config.common;
        let mut builder = MetricsBuilder::new(config);
        builder = setup_metrics_exporter(builder, &config.apollo, metrics_common_config)?;
        builder =
            setup_metrics_exporter(builder, &metrics_config.prometheus, metrics_common_config)?;
        builder = setup_metrics_exporter(builder, &metrics_config.otlp, metrics_common_config)?;
        Ok(builder)
    }

    fn filter_variables_values(
        variables: &Map<ByteString, Value>,
        forward_rules: &ForwardValues,
    ) -> String {
        let nb_var = variables.len();
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
            .fold(HashMap::with_capacity(nb_var), |mut acc, (name, value)| {
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
        result: Result<SupergraphResponse, BoxError>,
        request_duration: Duration,
    ) -> Result<SupergraphResponse, BoxError> {
        let mut metric_attrs = {
            context
                .extensions()
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

                let attributes = config
                    .exporters
                    .metrics
                    .common
                    .attributes
                    .supergraph
                    .get_attributes_from_router_response(&parts, &context, &first_response);

                metric_attrs.extend(attributes.into_iter().map(|(k, v)| KeyValue::new(k, v)));

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
        u64_counter!(
            "apollo_router_http_requests_total",
            "Total number of HTTP requests made.",
            1,
            metric_attrs
        );

        f64_histogram!(
            "apollo_router_http_request_duration_seconds",
            "Duration of HTTP requests.",
            request_duration.as_secs_f64(),
            metric_attrs
        );
        res
    }

    fn populate_context(
        config: Arc<Conf>,
        field_level_instrumentation_ratio: f64,
        req: &SupergraphRequest,
    ) {
        let context = &req.context;
        let http_request = &req.supergraph_request;
        let headers = http_request.headers();

        let (should_log_headers, should_log_body) = config.exporters.logging.should_log(req);
        if should_log_headers {
            let sorted_headers = req
                .supergraph_request
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect::<BTreeMap<_, _>>();
            ::tracing::info!(http.request.headers = ?sorted_headers, "Supergraph request headers");

            let _ = req.context.insert(LOGGING_DISPLAY_HEADERS, true);
        }
        if should_log_body {
            ::tracing::info!(http.request.body = ?req.supergraph_request.body(), "Supergraph request body");

            let _ = req.context.insert(LOGGING_DISPLAY_BODY, true);
        }

        // List of custom attributes for metrics
        let mut attributes: HashMap<String, AttributeValue> = HashMap::new();
        if let Some(operation_name) = &req.supergraph_request.body().operation_name {
            attributes.insert(
                OPERATION_NAME.to_string(),
                AttributeValue::String(operation_name.clone()),
            );
        }

        let router_attributes_conf = &config.exporters.metrics.common.attributes.supergraph;
        attributes.extend(
            router_attributes_conf
                .get_attributes_from_request(headers, req.supergraph_request.body()),
        );
        attributes.extend(router_attributes_conf.get_attributes_from_context(context));

        let _ = context
            .extensions()
            .lock()
            .insert(MetricsAttributes(attributes));
        if rand::thread_rng().gen_bool(field_level_instrumentation_ratio) {
            context.extensions().lock().insert(EnableSubgraphFtv1);
        }
    }

    fn create_subgraph_metrics_conf(&self, name: &str) -> Arc<AttributesForwardConf> {
        let subgraph_cfg = &self.config.exporters.metrics.common.attributes.subgraph;
        macro_rules! extend_config {
            ($forward_kind: ident) => {{
                let mut cfg = subgraph_cfg.all.$forward_kind.clone();
                cfg.extend(
                    subgraph_cfg
                        .subgraphs
                        .get(&name.to_owned())
                        .map(|s| s.$forward_kind.clone())
                        .unwrap_or_default(),
                );

                cfg
            }};
        }
        macro_rules! merge_config {
            ($forward_kind: ident) => {{
                let mut cfg = subgraph_cfg.all.$forward_kind.clone();
                cfg.merge(
                    subgraph_cfg
                        .subgraphs
                        .get(&name.to_owned())
                        .map(|s| s.$forward_kind.clone())
                        .unwrap_or_default(),
                );

                cfg
            }};
        }

        Arc::new(AttributesForwardConf {
            insert: extend_config!(insert),
            request: merge_config!(request),
            response: merge_config!(response),
            errors: merge_config!(errors),
            context: extend_config!(context),
        })
    }

    fn store_subgraph_request_attributes(
        attribute_forward_config: &AttributesForwardConf,
        sub_request: &Request,
    ) {
        let mut attributes = HashMap::new();
        attributes.extend(attribute_forward_config.get_attributes_from_request(
            sub_request.subgraph_request.headers(),
            sub_request.subgraph_request.body(),
        ));
        attributes
            .extend(attribute_forward_config.get_attributes_from_context(&sub_request.context));
        sub_request
            .context
            .extensions()
            .lock()
            .insert(SubgraphMetricsAttributes(attributes)); //.unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn store_subgraph_response_attributes(
        context: &Context,
        subgraph_attribute: KeyValue,
        attribute_forward_config: &AttributesForwardConf,
        now: Instant,
        result: &Result<Response, BoxError>,
    ) {
        let mut metric_attrs = {
            context
                .extensions()
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
        metric_attrs.extend(
            attribute_forward_config
                .get_attributes_from_context(context)
                .into_iter()
                .map(|(k, v)| KeyValue::new(k, v)),
        );

        match &result {
            Ok(response) => {
                metric_attrs.push(KeyValue::new(
                    "status",
                    response.response.status().as_u16().to_string(),
                ));

                // Fill attributes from response
                metric_attrs.extend(
                    attribute_forward_config
                        .get_attributes_from_response(
                            response.response.headers(),
                            response.response.body(),
                        )
                        .into_iter()
                        .map(|(k, v)| KeyValue::new(k, v)),
                );

                u64_counter!(
                    "apollo_router_http_requests_total",
                    "Total number of HTTP requests made.",
                    1,
                    metric_attrs
                );
            }
            Err(err) => {
                metric_attrs.push(KeyValue::new("status", "500"));
                // Fill attributes from error
                metric_attrs.extend(
                    attribute_forward_config
                        .get_attributes_from_error(err)
                        .into_iter()
                        .map(|(k, v)| KeyValue::new(k, v)),
                );

                u64_counter!(
                    "apollo_router_http_requests_total",
                    "Total number of HTTP requests made.",
                    1,
                    metric_attrs
                );
            }
        }
        f64_histogram!(
            "apollo_router_http_request_duration_seconds",
            "Duration of HTTP requests.",
            now.elapsed().as_secs_f64(),
            metric_attrs
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn update_metrics_on_response_events(
        ctx: &Context,
        config: Arc<Conf>,
        field_level_instrumentation_ratio: f64,
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

                metric_attrs.extend(
                    config
                        .exporters
                        .metrics
                        .common
                        .attributes
                        .supergraph
                        .get_attributes_from_error(&e)
                        .into_iter()
                        .map(|(k, v)| KeyValue::new(k, v)),
                );

                u64_counter!(
                    "apollo_router_http_requests_total",
                    "Total number of HTTP requests made.",
                    1,
                    metric_attrs
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
                            // Useful for selector in spans/instruments/events
                            ctx.insert_json_value(
                                CONTAINS_GRAPHQL_ERROR,
                                serde_json_bytes::Value::Bool(has_errors),
                            );

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
        let metrics = if let Some(usage_reporting) = {
            let lock = context.extensions().lock();
            let urp = lock.get::<Arc<UsageReporting>>();
            urp.cloned()
        } {
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
                                .clone()
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
        let metrics_prom_used = config.exporters.metrics.prometheus.enabled;
        let metrics_otlp_used = MetricsConfigurator::enabled(&config.exporters.metrics.otlp);
        let tracing_otlp_used = TracingConfigurator::enabled(&config.exporters.tracing.otlp);
        let tracing_datadog_used = config.exporters.tracing.datadog.enabled();
        let tracing_jaeger_used = config.exporters.tracing.jaeger.enabled();
        let tracing_zipkin_used = config.exporters.tracing.zipkin.enabled();

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

    fn checked_tracer_shutdown(tracer_provider: opentelemetry::sdk::trace::TracerProvider) {
        Self::checked_spawn_task(Box::new(move || {
            drop(tracer_provider);
        }));
    }

    fn checked_global_tracer_shutdown(global_tracer_provider: GlobalTracerProvider) {
        Self::checked_spawn_task(Box::new(move || {
            drop(global_tracer_provider);
        }));
    }

    fn checked_spawn_task(task: Box<dyn FnOnce() + Send + 'static>) {
        // If we are in an tokio async context, use `spawn_blocking()`, if not just execute the
        // task.
        // Note:
        //  - If we use spawn_blocking, then tokio looks after waiting for the task to
        //    terminate
        //  - We could spawn a thread to execute the task, but if the process terminated that would
        //    cause the thread to terminate which isn't ideal. Let's just run it in the current
        //    thread. This won't affect router performance since that will always be within the
        //    context of tokio.
        match Handle::try_current() {
            Ok(hdl) => {
                hdl.spawn_blocking(move || {
                    task();
                });
                // We don't join here since we can't await or block_on()
            }
            Err(_err) => {
                task();
            }
        }
    }
}

impl TelemetryActivation {
    fn reload_metrics(&mut self) {
        let meter_provider = meter_provider();
        commit_prometheus();
        let mut old_meter_providers: [Option<FilterMeterProvider>; 3] = Default::default();

        old_meter_providers[0] = meter_provider.set(
            MeterProviderType::PublicPrometheus,
            self.public_prometheus_meter_provider.take(),
        );

        old_meter_providers[1] = meter_provider.set(
            MeterProviderType::Apollo,
            self.private_meter_provider.take(),
        );

        old_meter_providers[2] =
            meter_provider.set(MeterProviderType::Public, self.public_meter_provider.take());

        metrics_layer().clear();

        Self::checked_meter_shutdown(old_meter_providers);
    }

    fn checked_meter_shutdown(meters: [Option<FilterMeterProvider>; 3]) {
        for meter_provider in meters.into_iter().flatten() {
            Telemetry::checked_spawn_task(Box::new(move || {
                if let Err(e) = meter_provider.shutdown() {
                    ::tracing::error!(error = %e, "failed to shutdown meter provider")
                }
            }));
        }
    }
}

fn filter_headers(headers: &HeaderMap, forward_rules: &ForwardHeaders) -> String {
    if let ForwardHeaders::None = forward_rules {
        return String::from("{}");
    }
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
        .fold(
            BTreeMap::new(),
            |mut acc: BTreeMap<String, Vec<String>>, (name, value)| {
                acc.entry(name).or_default().push(value);
                acc
            },
        );

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

    handle_error_internal(err, last_logged_map);
}

fn handle_error_internal<T: Into<opentelemetry::global::Error>>(
    err: T,
    last_logged_map: &DashMap<ErrorType, Instant>,
) {
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

    // For now we have to suppress Metrics error: reader is shut down or not registered
    // https://github.com/open-telemetry/opentelemetry-rust/issues/1244
    if let opentelemetry::global::Error::Metric(err) = &err {
        if err.to_string() == "Metrics error: reader is shut down or not registered" {
            return;
        }
    }
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
        .extensions()
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
        .extensions()
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
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use axum::headers::HeaderName;
    use dashmap::DashMap;
    use http::header::CONTENT_TYPE;
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
    use super::Telemetry;
    use crate::error::FetchError;
    use crate::graphql;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::json_ext::Object;
    use crate::metrics::FutureMetricsExt;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugin::DynPlugin;
    use crate::plugins::telemetry::handle_error_internal;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::SupergraphRequest;
    use crate::services::SupergraphResponse;

    async fn create_plugin_with_config(config: &str) -> Box<dyn DynPlugin> {
        let prometheus_support = config.contains("prometheus");
        let config: Value = serde_yaml::from_str(config).expect("yaml must be valid");
        let telemetry_config = config
            .as_object()
            .expect("must be an object")
            .get("telemetry")
            .expect("root key must be telemetry");
        let mut plugin = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance_without_schema(telemetry_config)
            .await
            .unwrap();

        if prometheus_support {
            plugin
                .as_any_mut()
                .downcast_mut::<Telemetry>()
                .unwrap()
                .activation
                .lock()
                .reload_metrics();
        }
        plugin
    }

    async fn get_prometheus_metrics(plugin: &dyn DynPlugin) -> String {
        let web_endpoint = plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();

        let http_req_prom = http::Request::get("http://localhost:9090/metrics")
            .body(Default::default())
            .unwrap();
        let mut resp = web_endpoint.oneshot(http_req_prom).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(resp.body_mut()).await.unwrap();
        String::from_utf8_lossy(&body)
            .to_string()
            .split('\n')
            .filter(|l| l.contains("bucket") && !l.contains("apollo_router_span_count"))
            .sorted()
            .join("\n")
    }

    async fn make_supergraph_request(plugin: &dyn DynPlugin) {
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

        let mut supergraph_service = plugin.supergraph_service(BoxService::new(mock_service));
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
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_registered() {
        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &serde_json::json!({"apollo": {"schema_id":"abc"}, "exporters": {"tracing": {}}}),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn config_serialization() {
        create_plugin_with_config(include_str!("testdata/config.router.yaml")).await;
    }

    #[tokio::test]
    async fn test_supergraph_metrics_ok() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml"))
                    .await;
            make_supergraph_request(plugin.as_ref()).await;

            assert_counter!(
                "apollo_router_http_requests_total",
                1,
                "another_test" = "my_default_value",
                "my_value" = 2,
                "myname" = "label_value",
                "renamed_value" = "my_value_set",
                "status" = "200",
                "x-custom" = "coming_from_header"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_supergraph_metrics_bad_request() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml"))
                    .await;

            let mut mock_bad_request_service = MockSupergraphService::new();
            mock_bad_request_service.expect_call().times(1).returning(
                move |req: SupergraphRequest| {
                    Ok(SupergraphResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .data(json!({"errors": [{"message": "nope"}]}))
                        .build()
                        .unwrap())
                },
            );
            let mut bad_request_supergraph_service =
                plugin.supergraph_service(BoxService::new(mock_bad_request_service));
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

            assert_counter!(
                "apollo_router_http_requests_total",
                1,
                "another_test" = "my_default_value",
                "error" = "400 Bad Request",
                "myname" = "label_value",
                "renamed_value" = "my_value_set",
                "status" = "400"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_custom_router_instruments() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_instruments.router.yaml"))
                    .await;

            let mut mock_bad_request_service = MockRouterService::new();
            mock_bad_request_service
                .expect_call()
                .times(2)
                .returning(move |req: RouterRequest| {
                    Ok(RouterResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .header("content-type", "application/json")
                        .data(json!({"errors": [{"message": "nope"}]}))
                        .build()
                        .unwrap())
                });
            let mut bad_request_router_service =
                plugin.router_service(BoxService::new(mock_bad_request_service));
            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();

            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                55.0,
                "http.response.status_code" = 400,
                "acme.my_attribute" = "application/json"
            );
            assert_histogram_sum!("acme.request.length", 55.0);

            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!("acme.request.length", 60.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                60.0,
                "http.response.status_code" = 400,
                "acme.my_attribute" = "application/json"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_custom_router_instruments_with_requirement_level() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/custom_instruments_level.router.yaml"
            ))
            .await;

            let mut mock_bad_request_service = MockRouterService::new();
            mock_bad_request_service
                .expect_call()
                .times(2)
                .returning(move |req: RouterRequest| {
                    Ok(RouterResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .header("content-type", "application/json")
                        .data(json!({"errors": [{"message": "nope"}]}))
                        .build()
                        .unwrap())
                });
            let mut bad_request_router_service =
                plugin.router_service(BoxService::new(mock_bad_request_service));
            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();

            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                55.0,
                "acme.my_attribute" = "application/json",
                "error.type" = "Bad Request",
                "http.response.status_code" = 400,
                "network.protocol.version" = "HTTP/1.1"
            );
            assert_histogram_exists!(
                "http.server.request.duration",
                f64,
                "error.type" = "Bad Request",
                "http.response.status_code" = 400,
                "network.protocol.version" = "HTTP/1.1",
                "http.request.method" = "GET"
            );
            assert_histogram_sum!("acme.request.length", 55.0);

            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!("acme.request.length", 60.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                60.0,
                "http.response.status_code" = 400,
                "acme.my_attribute" = "application/json",
                "error.type" = "Bad Request",
                "http.response.status_code" = 400,
                "network.protocol.version" = "HTTP/1.1"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_custom_supergraph_instruments() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_instruments.router.yaml"))
                    .await;

            let mut mock_bad_request_service = MockSupergraphService::new();
            mock_bad_request_service.expect_call().times(3).returning(
                move |req: SupergraphRequest| {
                    Ok(SupergraphResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .header("content-type", "application/json")
                        .data(json!({"errors": [{"message": "nope"}]}))
                        .build()
                        .unwrap())
                },
            );
            let mut bad_request_supergraph_service =
                plugin.supergraph_service(BoxService::new(mock_bad_request_service));
            let supergraph_req = SupergraphRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());
            let _router_response = bad_request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();

            assert_counter!(
                "acme.graphql.requests",
                1.0,
                "acme.my_attribute" = "application/json",
                "graphql_query" = "Query test { me {name} }",
                "graphql.document" = "Query test { me {name} }"
            );

            let supergraph_req = SupergraphRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());

            let _router_response = bad_request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!(
                "acme.graphql.requests",
                2.0,
                "acme.my_attribute" = "application/json",
                "graphql_query" = "Query test { me {name} }",
                "graphql.document" = "Query test { me {name} }"
            );

            let supergraph_req = SupergraphRequest::fake_builder()
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());

            let _router_response = bad_request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!(
                "acme.graphql.requests",
                2.0,
                "acme.my_attribute" = "application/json",
                "graphql_query" = "Query test { me {name} }",
                "graphql.document" = "Query test { me {name} }"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_custom_subgraph_instruments_level() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/custom_instruments_level.router.yaml"
            ))
            .await;

            let mut mock_bad_request_service = MockSubgraphService::new();
            mock_bad_request_service.expect_call().times(2).returning(
                move |req: SubgraphRequest| {
                    let mut headers = HeaderMap::new();
                    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
                    let errors = vec![
                        graphql::Error::builder()
                            .message("nope".to_string())
                            .extension_code("NOPE")
                            .build(),
                        graphql::Error::builder()
                            .message("nok".to_string())
                            .extension_code("NOK")
                            .build(),
                    ];
                    Ok(SubgraphResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .headers(headers)
                        .errors(errors)
                        .build())
                },
            );
            let mut bad_request_subgraph_service =
                plugin.subgraph_service("test", BoxService::new(mock_bad_request_service));
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();

            assert_counter!(
                "acme.subgraph.error_reqs",
                1.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();
            assert_counter!(
                "acme.subgraph.error_reqs",
                2.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
            assert_histogram_not_exists!("http.client.request.duration", f64);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_custom_subgraph_instruments() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_instruments.router.yaml"))
                    .await;

            let mut mock_bad_request_service = MockSubgraphService::new();
            mock_bad_request_service.expect_call().times(2).returning(
                move |req: SubgraphRequest| {
                    let mut headers = HeaderMap::new();
                    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
                    let errors = vec![
                        graphql::Error::builder()
                            .message("nope".to_string())
                            .extension_code("NOPE")
                            .build(),
                        graphql::Error::builder()
                            .message("nok".to_string())
                            .extension_code("NOK")
                            .build(),
                    ];
                    Ok(SubgraphResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .headers(headers)
                        .errors(errors)
                        .build())
                },
            );
            let mut bad_request_subgraph_service =
                plugin.subgraph_service("test", BoxService::new(mock_bad_request_service));
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();

            assert_counter!(
                "acme.subgraph.error_reqs",
                1.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();
            assert_counter!(
                "acme.subgraph.error_reqs",
                2.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_subgraph_metrics_ok() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml"))
                    .await;

            let mut mock_subgraph_service = MockSubgraphService::new();
            mock_subgraph_service
                .expect_call()
                .times(1)
                .returning(move |req: SubgraphRequest| {
                    let mut extension = Object::new();
                    extension.insert(
                        serde_json_bytes::ByteString::from("status"),
                        serde_json_bytes::Value::String(ByteString::from(
                            "custom_error_for_propagation",
                        )),
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

            let mut subgraph_service =
                plugin.subgraph_service("my_subgraph_name", BoxService::new(mock_subgraph_service));
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

            assert_counter!(
                "apollo_router_http_requests_total",
                1,
                "error" = "custom_error_for_propagation",
                "my_key" = "my_custom_attribute_from_context",
                "query_from_request" = "query { test }",
                "status" = "200",
                "subgraph" = "my_subgraph_name",
                "unknown_data" = "default_value"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_subgraph_metrics_http_error() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml"))
                    .await;

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

            let mut subgraph_service = plugin.subgraph_service(
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
                .expect_err("should be an error");

            assert_counter!(
                "apollo_router_http_requests_total",
                1,
                "message" = "cannot contact the subgraph",
                "status" = "500",
                "subgraph" = "my_subgraph_name_error",
                "subgraph_error_extended_code" = "SUBREQUEST_HTTP_ERROR"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_subgraph_metrics_bad_request() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml"))
                    .await;

            let mut mock_bad_request_service = MockSupergraphService::new();
            mock_bad_request_service.expect_call().times(1).returning(
                move |req: SupergraphRequest| {
                    Ok(SupergraphResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .data(json!({"errors": [{"message": "nope"}]}))
                        .build()
                        .unwrap())
                },
            );

            let mut bad_request_supergraph_service =
                plugin.supergraph_service(BoxService::new(mock_bad_request_service));

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

            assert_counter!(
                "apollo_router_http_requests_total",
                1,
                "another_test" = "my_default_value",
                "error" = "400 Bad Request",
                "myname" = "label_value",
                "renamed_value" = "my_value_set",
                "status" = "400"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn it_test_prometheus_wrong_endpoint() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/prometheus.router.yaml")).await;

            let mut web_endpoint = plugin
                .web_endpoints()
                .into_iter()
                .next()
                .unwrap()
                .1
                .into_iter()
                .next()
                .unwrap()
                .into_router();

            let http_req_prom = http::Request::get("http://localhost:9090/WRONG/URL/metrics")
                .body(Default::default())
                .unwrap();

            let resp = web_endpoint
                .ready()
                .await
                .unwrap()
                .call(http_req_prom)
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/prometheus.router.yaml")).await;
            make_supergraph_request(plugin.as_ref()).await;
            let prometheus_metrics = get_prometheus_metrics(plugin.as_ref()).await;
            assert_snapshot!(prometheus_metrics);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_custom_buckets() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/prometheus_custom_buckets.router.yaml"
            ))
            .await;
            make_supergraph_request(plugin.as_ref()).await;
            let prometheus_metrics = get_prometheus_metrics(plugin.as_ref()).await;

            assert_snapshot!(prometheus_metrics);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_custom_buckets_for_specific_metrics() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/prometheus_custom_buckets_specific_metrics.router.yaml"
            ))
            .await;
            make_supergraph_request(plugin.as_ref()).await;
            let prometheus_metrics = get_prometheus_metrics(plugin.as_ref()).await;

            assert_snapshot!(prometheus_metrics);
        }
        .with_metrics()
        .await;
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
        let error_map = DashMap::new();
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
            handle_error_internal(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
            handle_error_internal(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
            handle_error_internal(
                opentelemetry::global::Error::Trace("trace error".to_string().into()),
                &error_map,
            );
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;

        test_layer.assert_log_entry_count("other error", 1);
        test_layer.assert_log_entry_count("trace error", 1);

        // Sleep a bit and then log again, it should get logged
        tokio::time::sleep(Duration::from_millis(200)).await;
        async {
            handle_error_internal(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;
        test_layer.assert_log_entry_count("other error", 2);
    }
}
