//! Telemetry plugin.
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use std::time::Instant;

use ::tracing::Span;
use ::tracing::info_span;
use axum_extra::headers::HeaderName;
use config_new::Selectors;
use config_new::cache::CacheInstruments;
use config_new::connector::instruments::ConnectorInstruments;
use config_new::instruments::InstrumentsConfig;
use config_new::instruments::StaticInstrument;
use error_handler::handle_error;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::future::ready;
use futures::stream::once;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use http::header;
use http::header::CACHE_CONTROL;
use metrics::apollo::studio::SingleLimitsStats;
use metrics::local_type_stats::LocalTypeStatRecorder;
use multimap::MultiMap;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::global::GlobalTracerProvider;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::propagation::Extractor;
use opentelemetry::propagation::Injector;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::propagation::text_map_propagator::FieldIter;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceId;
use opentelemetry::trace::TraceState;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::Builder;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use parking_lot::Mutex;
use parking_lot::RwLock;
use rand::Rng;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use tokio::runtime::Handle;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use uuid::Uuid;

use self::apollo::ForwardValues;
use self::apollo::LicensedOperationCountByType;
use self::apollo::OperationSubType;
use self::apollo::SingleReport;
use self::apollo_exporter::Sender;
use self::apollo_exporter::proto;
use self::config::Conf;
use self::config::TraceIdFormat;
use self::config_new::instruments::Instrumented;
use self::config_new::router::events::RouterEvents;
use self::config_new::router::instruments::RouterInstruments;
use self::config_new::spans::Spans;
use self::config_new::subgraph::events::SubgraphEvents;
use self::config_new::subgraph::instruments::SubgraphInstruments;
use self::config_new::supergraph::events::SupergraphEvents;
use self::metrics::apollo::studio::SingleTypeStat;
use self::reload::reload_fmt;
pub(crate) use self::span_factory::SpanMode;
use self::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use self::tracing::apollo_telemetry::CLIENT_NAME_KEY;
use self::tracing::apollo_telemetry::CLIENT_VERSION_KEY;
use crate::Context;
use crate::ListenAddr;
use crate::apollo_studio_interop::ExtendedReferenceStats;
use crate::apollo_studio_interop::ReferencedEnums;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::graphql::ResponseVisitor;
use crate::layers::ServiceBuilderExt;
use crate::layers::instrument::InstrumentLayer;
use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::metrics::meter_provider;
use crate::metrics::meter_provider_internal;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::telemetry::apollo::ForwardHeaders;
use crate::plugins::telemetry::apollo_exporter::proto::reports::StatsContext;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::node::Id::ResponseName;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloConnectorInstruments;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloSubgraphInstruments;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEvents;
use crate::plugins::telemetry::config_new::cost::add_cost_attributes;
use crate::plugins::telemetry::config_new::graphql::GraphQLInstruments;
use crate::plugins::telemetry::config_new::instruments::SupergraphInstruments;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::consts::EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::consts::OTEL_NAME;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::plugins::telemetry::consts::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::error_counter::count_execution_errors;
use crate::plugins::telemetry::error_counter::count_router_errors;
use crate::plugins::telemetry::error_counter::count_subgraph_errors;
use crate::plugins::telemetry::error_counter::count_supergraph_errors;
use crate::plugins::telemetry::fmt_layer::create_fmt_layer;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::metrics::apollo::histogram::ListLengthHistogram;
use crate::plugins::telemetry::metrics::apollo::studio::LocalTypeStat;
use crate::plugins::telemetry::metrics::apollo::studio::SingleContextualizedStats;
use crate::plugins::telemetry::metrics::apollo::studio::SinglePathErrorStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleQueryLatencyStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStatsReport;
use crate::plugins::telemetry::metrics::prometheus::commit_prometheus;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::reload::OPENTELEMETRY_TRACER_HANDLE;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;
use crate::plugins::telemetry::tracing::apollo_telemetry::decode_ftv1_trace;
use crate::query_planner::OperationKind;
use crate::register_private_plugin;
use crate::router_factory::Endpoint;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::connector;
use crate::services::execution;
use crate::services::layers::apq::PERSISTED_QUERY_CACHE_HIT;
use crate::services::layers::persisted_queries::RequestPersistedQueryId;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::operation_limits::OperationLimits;

pub(crate) mod apollo;
pub(crate) mod apollo_exporter;
pub(crate) mod apollo_otlp_exporter;
pub(crate) mod config;
pub(crate) mod config_new;
pub(crate) mod consts;
pub(crate) mod dynamic_attribute;
mod endpoint;
mod error_counter;
mod error_handler;
mod fmt_layer;
pub(crate) mod formatters;
mod logging;
pub(crate) mod metrics;
/// Opentelemetry utils
pub(crate) mod otel;
mod otlp;
pub(crate) mod reload;
pub(crate) mod resource;
pub(crate) mod span_ext;
mod span_factory;
pub(crate) mod tracing;
pub(crate) mod utils;

// Tracing consts
pub(crate) const CLIENT_NAME: &str = "apollo::telemetry::client_name";
pub(crate) const CLIENT_LIBRARY_NAME: &str = "apollo::telemetry::client_library_name";
pub(crate) const CLIENT_VERSION: &str = "apollo::telemetry::client_version";
pub(crate) const CLIENT_LIBRARY_VERSION: &str = "apollo::telemetry::client_library_version";
pub(crate) const SUBGRAPH_FTV1: &str = "apollo::telemetry::subgraph_ftv1";
pub(crate) const STUDIO_EXCLUDE: &str = "apollo::telemetry::studio_exclude";
pub(crate) const SUPERGRAPH_SCHEMA_ID_CONTEXT_KEY: &str = "apollo::supergraph_schema_id";
const GLOBAL_TRACER_NAME: &str = "apollo-router";
const DEFAULT_EXPOSE_TRACE_ID_HEADER: &str = "apollo-trace-id";
static DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME: HeaderName =
    HeaderName::from_static(DEFAULT_EXPOSE_TRACE_ID_HEADER);
static FTV1_HEADER_NAME: HeaderName = HeaderName::from_static("apollo-federation-include-trace");
static FTV1_HEADER_VALUE: HeaderValue = HeaderValue::from_static("ftv1");

pub(crate) const APOLLO_PRIVATE_QUERY_ALIASES: Key =
    Key::from_static_str("apollo_private.query.aliases");
pub(crate) const APOLLO_PRIVATE_QUERY_DEPTH: Key =
    Key::from_static_str("apollo_private.query.depth");
pub(crate) const APOLLO_PRIVATE_QUERY_HEIGHT: Key =
    Key::from_static_str("apollo_private.query.height");
pub(crate) const APOLLO_PRIVATE_QUERY_ROOT_FIELDS: Key =
    Key::from_static_str("apollo_private.query.root_fields");

// Standard Apollo Otel Metric Attribute Names
pub(crate) const APOLLO_CLIENT_NAME_ATTRIBUTE: &str = "apollo.client.name";
pub(crate) const APOLLO_CLIENT_VERSION_ATTRIBUTE: &str = "apollo.client.version";
pub(crate) const GRAPHQL_OPERATION_NAME_ATTRIBUTE: &str = "graphql.operation.name";
pub(crate) const GRAPHQL_OPERATION_TYPE_ATTRIBUTE: &str = "graphql.operation.type";
pub(crate) const APOLLO_OPERATION_ID_ATTRIBUTE: &str = "apollo.operation.id";
pub(crate) const APOLLO_HAS_ERRORS_ATTRIBUTE: &str = "has_errors";
pub(crate) const APOLLO_CONNECTOR_SOURCE_ATTRIBUTE: &str = "connector.source";

#[doc(hidden)] // Only public for integration tests
pub(crate) struct Telemetry {
    pub(crate) config: Arc<config::Conf>,
    supergraph_schema_id: Arc<String>,
    custom_endpoints: MultiMap<ListenAddr, Endpoint>,
    apollo_metrics_sender: apollo_exporter::Sender,
    field_level_instrumentation_ratio: f64,
    builtin_instruments: RwLock<BuiltinInstruments>,
    activation: Mutex<TelemetryActivation>,
    enabled_features: EnabledFeatures,
}

struct TelemetryActivation {
    tracer_provider: Option<opentelemetry_sdk::trace::TracerProvider>,
    // We have to have separate meter providers for prometheus metrics so that they don't get zapped on router reload.
    public_meter_provider: Option<FilterMeterProvider>,
    public_prometheus_meter_provider: Option<FilterMeterProvider>,
    private_meter_provider: Option<FilterMeterProvider>,
    private_realtime_meter_provider: Option<FilterMeterProvider>,
    is_active: bool,
}

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
        let metrics_providers: [Option<FilterMeterProvider>; 4] = [
            activation.private_realtime_meter_provider.take(),
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

/// When observed, it reports the most recently stored value (give or take atomicity looseness).
///
/// This *could* be generalised to any kind of gauge, but we should ideally have gauges that can just
/// observe their accurate value whenever requested. The externally updateable approach is kind of
/// a hack that happens to work here because we only have one place where the value can change, and
/// otherwise we might have to use an inconvenient Mutex or RwLock around the entire LRU cache.
#[derive(Debug, Clone)]
pub(crate) struct LruSizeInstrument {
    value: Arc<AtomicU64>,
    _gauge: ObservableGauge<u64>,
}

impl LruSizeInstrument {
    pub(crate) fn new(gauge_name: &'static str) -> Self {
        let value = Arc::new(AtomicU64::new(0));

        let meter = meter_provider().meter("apollo/router");
        let gauge = meter
            .u64_observable_gauge(gauge_name)
            .with_callback({
                let value = Arc::clone(&value);
                move |gauge| {
                    gauge.observe(value.load(std::sync::atomic::Ordering::Relaxed), &[]);
                }
            })
            .init();

        Self {
            value,
            _gauge: gauge,
        }
    }

    pub(crate) fn update(&self, value: u64) {
        self.value
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }
}

struct BuiltinInstruments {
    graphql_custom_instruments: Arc<HashMap<String, StaticInstrument>>,
    router_custom_instruments: Arc<HashMap<String, StaticInstrument>>,
    supergraph_custom_instruments: Arc<HashMap<String, StaticInstrument>>,
    subgraph_custom_instruments: Arc<HashMap<String, StaticInstrument>>,
    apollo_subgraph_instruments: Arc<HashMap<String, StaticInstrument>>,
    connector_custom_instruments: Arc<HashMap<String, StaticInstrument>>,
    apollo_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
    cache_custom_instruments: Arc<HashMap<String, StaticInstrument>>,
    _pipeline_instruments: Arc<HashMap<String, StaticInstrument>>,
}

fn create_builtin_instruments(config: &InstrumentsConfig) -> BuiltinInstruments {
    BuiltinInstruments {
        graphql_custom_instruments: Arc::new(config.new_builtin_graphql_instruments()),
        router_custom_instruments: Arc::new(config.new_builtin_router_instruments()),
        supergraph_custom_instruments: Arc::new(config.new_builtin_supergraph_instruments()),
        subgraph_custom_instruments: Arc::new(config.new_builtin_subgraph_instruments()),
        apollo_subgraph_instruments: Arc::new(config.new_builtin_apollo_subgraph_instruments()),
        connector_custom_instruments: Arc::new(config.new_builtin_connector_instruments()),
        apollo_connector_instruments: Arc::new(config.new_builtin_apollo_connector_instruments()),
        cache_custom_instruments: Arc::new(config.new_builtin_cache_instruments()),
        _pipeline_instruments: Arc::new(config.new_pipeline_instruments()),
    }
}

#[derive(Clone, Debug)]
struct EnabledFeatures {
    distributed_apq_cache: bool,
    entity_cache: bool,
}

impl EnabledFeatures {
    fn list(&self) -> Vec<String> {
        // Map enabled features to their names for usage reports
        [
            ("distributed_apq_cache", self.distributed_apq_cache),
            ("entity_cache", self.entity_cache),
        ]
        .iter()
        .filter(|&&(_, enabled)| enabled)
        .map(&|(name, _): &(&str, _)| name.to_string())
        .collect()
    }
}

#[async_trait::async_trait]
impl PluginPrivate for Telemetry {
    type Config = config::Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        // Log whether we received previous configuration for testing
        // In a followup PR we will be detecting if exporters need to be refreshed, and at this point
        // this debug logging will disappear.
        match &init.previous_config {
            Some(_prev_config) => {
                ::tracing::debug!("Telemetry plugin reload detected with previous configuration");
            }
            None => {
                ::tracing::debug!(
                    "Telemetry plugin initial startup without previous configuration"
                );
            }
        }

        opentelemetry::global::set_error_handler(handle_error)
            .expect("otel error handler lock poisoned, fatal");

        let mut config = init.config;
        config.instrumentation.spans.update_defaults();
        config.instrumentation.instruments.update_defaults();
        if let Err(err) = config.instrumentation.validate() {
            ::tracing::warn!(
                "Potential configuration error for 'instrumentation': {err}, please check the documentation on https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/events"
            );
        }

        // Validate that Datadog and trace context propagation are not both active
        Self::validate_propagation_compatibility(&config)?;

        let field_level_instrumentation_ratio =
            config.calculate_field_level_instrumentation_ratio()?;
        let metrics_builder = Self::create_metrics_builder(&config)?;
        let tracer_provider = Self::create_tracer_provider(&config)?;

        if config.instrumentation.spans.mode == SpanMode::Deprecated {
            ::tracing::warn!(
                "telemetry.instrumentation.spans.mode is currently set to 'deprecated', either explicitly or via defaulting. Set telemetry.instrumentation.spans.mode explicitly in your router.yaml to 'spec_compliant' for log and span attributes that follow OpenTelemetry semantic conventions. This option will be defaulted to 'spec_compliant' in a future release and eventually removed altogether"
            );
        }

        // Set up feature usage list
        let full_config = init
            .full_config
            .as_ref()
            .expect("Required full router configuration not found in telemetry plugin");
        let enabled_features = Self::extract_enabled_features(full_config);
        ::tracing::debug!("Enabled scale features: {:?}", enabled_features);

        Ok(Telemetry {
            custom_endpoints: metrics_builder.custom_endpoints,
            apollo_metrics_sender: metrics_builder.apollo_metrics_sender,
            supergraph_schema_id: init.supergraph_schema_id,
            field_level_instrumentation_ratio,
            activation: Mutex::new(TelemetryActivation {
                tracer_provider: Some(tracer_provider),
                public_meter_provider: Some(FilterMeterProvider::public(
                    metrics_builder.public_meter_provider_builder.build(),
                )),
                private_meter_provider: Some(FilterMeterProvider::private(
                    metrics_builder.apollo_meter_provider_builder.build(),
                )),
                private_realtime_meter_provider: Some(FilterMeterProvider::private_realtime(
                    metrics_builder
                        .apollo_realtime_meter_provider_builder
                        .build(),
                )),
                public_prometheus_meter_provider: metrics_builder
                    .prometheus_meter_provider
                    .map(FilterMeterProvider::public),
                is_active: false,
            }),
            builtin_instruments: RwLock::new(create_builtin_instruments(
                &config.instrumentation.instruments,
            )),
            enabled_features,
            config: Arc::new(config),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let config = self.config.clone();
        let supergraph_schema_id = self.supergraph_schema_id.clone();
        let config_later = self.config.clone();
        let config_request = self.config.clone();
        let span_mode = config.instrumentation.spans.mode;
        let use_legacy_request_span =
            matches!(config.instrumentation.spans.mode, SpanMode::Deprecated);
        let enabled_features = self.enabled_features.clone();
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        let metrics_sender = self.apollo_metrics_sender.clone();
        let static_router_instruments = self
            .builtin_instruments
            .read()
            .router_custom_instruments
            .clone();

        ServiceBuilder::new()
            .map_response(move |response: router::Response| {
                // The current span *should* be the request span as we are outside the instrument block.
                let span = Span::current();
                if let Some(span_name) = span.metadata().map(|metadata| metadata.name())
                    && ((use_legacy_request_span && span_name == REQUEST_SPAN_NAME)
                        || (!use_legacy_request_span && span_name == ROUTER_SPAN_NAME))
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
                        (Ok(Some(kind)), Ok(Some(name))) => span.set_span_dyn_attribute(
                            OTEL_NAME.into(),
                            format!("{kind} {name}").into(),
                        ),
                        (Ok(Some(kind)), _) => {
                            span.set_span_dyn_attribute(OTEL_NAME.into(), kind.clone().into())
                        }
                        _ => span
                            .set_span_dyn_attribute(OTEL_NAME.into(), "GraphQL Operation".into()),
                    };
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
                    let _ = request.context.insert(
                        SUPERGRAPH_SCHEMA_ID_CONTEXT_KEY,
                        supergraph_schema_id.clone(),
                    );
                    if !use_legacy_request_span {
                        let span = Span::current();

                        span.set_span_dyn_attribute(
                            HTTP_REQUEST_METHOD.into(),
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

                    custom_attributes.push(KeyValue::new(
                        Key::from_static_str("apollo_private.http.request_headers"),
                        filter_headers(
                            request.router_request.headers(),
                            &config_request.apollo.send_headers,
                        ),
                    ));

                    let custom_instruments: RouterInstruments = config_request
                        .instrumentation
                        .instruments
                        .new_router_instruments(static_router_instruments.clone());
                    custom_instruments.on_request(request);

                    let mut custom_events: RouterEvents =
                        config_request.instrumentation.events.new_router_events();
                    custom_events.on_request(request);

                    (
                        custom_attributes,
                        custom_instruments,
                        custom_events,
                        request.context.clone(),
                    )
                },
                move |(mut custom_attributes, custom_instruments, mut custom_events, ctx): (
                    Vec<KeyValue>,
                    RouterInstruments,
                    RouterEvents,
                    Context,
                ),
                      fut| {
                    let start = Instant::now();
                    let config = config_later.clone();
                    let sender = metrics_sender.clone();
                    let enabled_features = enabled_features.clone();

                    Self::plugin_metrics(&config);

                    async move {
                        // NB: client name and version must be picked up here, rather than in the
                        //  `req_fn` of this `map_future_with_request_data` call, to allow plugins
                        //  at the router service to modify the name and version.
                        let get_from_context =
                            |ctx: &Context, key| ctx.get::<&str, String>(key).ok().flatten();
                        let client_name = get_from_context(&ctx, CLIENT_NAME).or_else(|| {
                            get_from_context(
                                &ctx,
                                crate::context::deprecated::DEPRECATED_CLIENT_NAME,
                            )
                        });
                        let client_version = get_from_context(&ctx, CLIENT_VERSION).or_else(|| {
                            get_from_context(
                                &ctx,
                                crate::context::deprecated::DEPRECATED_CLIENT_VERSION,
                            )
                        });
                        custom_attributes.extend([
                            KeyValue::new(CLIENT_NAME_KEY, client_name.unwrap_or_default()),
                            KeyValue::new(CLIENT_VERSION_KEY, client_version.unwrap_or_default()),
                        ]);

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

                            let mut headers: HashMap<String, Vec<String>> =
                                HashMap::with_capacity(2);
                            if expose_trace_id.enabled {
                                let header_name = expose_trace_id
                                    .header_name
                                    .as_ref()
                                    .unwrap_or(&DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME);

                                if let Some(value) = response.response.headers().get(header_name) {
                                    headers.insert(
                                        header_name.to_string(),
                                        vec![value.to_str().unwrap_or_default().to_string()],
                                    );
                                }
                            }
                            if let Some(value) = response.response.headers().get(&CACHE_CONTROL) {
                                headers.insert(
                                    CACHE_CONTROL.to_string(),
                                    vec![value.to_str().unwrap_or_default().to_string()],
                                );
                            }
                            if !headers.is_empty() {
                                let response_headers =
                                    serde_json::to_string(&headers).unwrap_or_default();
                                span.record(
                                    "apollo_private.http.response_headers",
                                    &response_headers,
                                );
                            }

                            if response.context.extensions().with_lock(|lock| {
                                lock.get::<Arc<UsageReporting>>()
                                    .map(|u| matches!(**u, UsageReporting::Error { .. }))
                                    .unwrap_or(false)
                            }) {
                                Self::update_apollo_metrics(
                                    &response.context,
                                    field_level_instrumentation_ratio,
                                    sender,
                                    true,
                                    start.elapsed(),
                                    // the query is invalid, we did not parse the operation kind
                                    OperationKind::Query,
                                    None,
                                    Default::default(),
                                    enabled_features.clone(),
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
                                config
                                    .instrumentation
                                    .spans
                                    .router
                                    .attributes
                                    .on_error(err, &ctx),
                            );
                            custom_instruments.on_error(err, &ctx);
                            custom_events.on_error(err, &ctx);
                        }

                        if let Ok(resp) = response {
                            Ok(count_router_errors(resp, &config.apollo.errors).await)
                        } else {
                            response
                        }
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
        let enabled_features = self.enabled_features.clone();
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        let static_supergraph_instruments = self
            .builtin_instruments
            .read()
            .supergraph_custom_instruments
            .clone();
        let static_graphql_instruments = self
            .builtin_instruments
            .read()
            .graphql_custom_instruments
            .clone();
        ServiceBuilder::new()
            .instrument(move |supergraph_req: &SupergraphRequest| {
                span_mode.create_supergraph(
                    &config_instrument.apollo,
                    supergraph_req,
                    field_level_instrumentation_ratio,
                )
            })
            .map_response(move |mut resp: SupergraphResponse| {
                let config = config_map_res_first.clone();
                if let Some(usage_reporting) = resp
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<Arc<UsageReporting>>().cloned())
                {
                    // Record the operation signature on the router span
                    Span::current().record(
                        APOLLO_PRIVATE_OPERATION_SIGNATURE.as_str(),
                        usage_reporting.get_stats_report_key().as_str(),
                    );
                }
                // To expose trace_id or not
                let expose_trace_id_header =
                    config.exporters.tracing.response_trace_id.enabled.then(|| {
                        config
                            .exporters
                            .tracing
                            .response_trace_id
                            .header_name
                            .clone()
                            .unwrap_or_else(|| DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME.clone())
                    });

                // Append the trace ID with the right format, based on the config
                let format_id = |trace_id: TraceId| {
                    let id = match config.exporters.tracing.response_trace_id.format {
                        TraceIdFormat::Hexadecimal | TraceIdFormat::OpenTelemetry => {
                            format!("{trace_id:032x}")
                        }
                        TraceIdFormat::Decimal => {
                            format!("{}", u128::from_be_bytes(trace_id.to_bytes()))
                        }
                        TraceIdFormat::Datadog => trace_id.to_datadog(),
                        TraceIdFormat::Uuid => Uuid::from_bytes(trace_id.to_bytes()).to_string(),
                    };

                    HeaderValue::from_str(&id).ok()
                };
                if let (Some(header_name), Some(trace_id)) =
                    (expose_trace_id_header, trace_id().and_then(format_id))
                {
                    resp.response.headers_mut().append(header_name, trace_id);
                }

                resp
            })
            .map_future_with_request_data(
                move |req: &SupergraphRequest| {
                    let custom_attributes = config
                        .instrumentation
                        .spans
                        .supergraph
                        .attributes
                        .on_request(req);
                    Self::populate_context(field_level_instrumentation_ratio, req);
                    let custom_instruments = config
                        .instrumentation
                        .instruments
                        .new_supergraph_instruments(static_supergraph_instruments.clone());
                    custom_instruments.on_request(req);
                    let custom_graphql_instruments: GraphQLInstruments = config
                        .instrumentation
                        .instruments
                        .new_graphql_instruments(static_graphql_instruments.clone());
                    custom_graphql_instruments.on_request(req);

                    let mut supergraph_events =
                        config.instrumentation.events.new_supergraph_events();
                    supergraph_events.on_request(req);

                    (
                        req.context.clone(),
                        custom_instruments,
                        custom_attributes,
                        supergraph_events,
                        custom_graphql_instruments,
                    )
                },
                move |(
                    ctx,
                    custom_instruments,
                    mut custom_attributes,
                    mut supergraph_events,
                    custom_graphql_instruments,
                ): (
                    Context,
                    SupergraphInstruments,
                    Vec<KeyValue>,
                    SupergraphEvents,
                    GraphQLInstruments,
                ),
                      fut| {
                    let config = config_map_res.clone();
                    let sender = metrics_sender.clone();
                    let enabled_features = enabled_features.clone();
                    let start = Instant::now();

                    async move {
                        let span = Span::current();
                        let mut result: Result<SupergraphResponse, BoxError> = fut.await;

                        add_query_attributes(&ctx, &mut custom_attributes);
                        add_cost_attributes(&ctx, &mut custom_attributes);
                        span.set_span_dyn_attributes(custom_attributes);
                        match &result {
                            Ok(resp) => {
                                span.set_span_dyn_attributes(
                                    config
                                        .instrumentation
                                        .spans
                                        .supergraph
                                        .attributes
                                        .on_response(resp),
                                );
                                custom_instruments.on_response(resp);
                                supergraph_events.on_response(resp);
                                custom_graphql_instruments.on_response(resp);
                            }
                            Err(err) => {
                                span.set_span_dyn_attributes(
                                    config
                                        .instrumentation
                                        .spans
                                        .supergraph
                                        .attributes
                                        .on_error(err, &ctx),
                                );
                                custom_instruments.on_error(err, &ctx);
                                supergraph_events.on_error(err, &ctx);
                                custom_graphql_instruments.on_error(err, &ctx);
                            }
                        }

                        if let Ok(resp) = result {
                            result = Ok(count_supergraph_errors(resp, &config.apollo.errors).await);
                        }

                        result = Self::update_otel_metrics(
                            config.clone(),
                            ctx.clone(),
                            result,
                            custom_instruments,
                            supergraph_events,
                            custom_graphql_instruments,
                        )
                        .await;
                        Self::update_metrics_on_response_events(
                            &ctx,
                            config,
                            field_level_instrumentation_ratio,
                            sender,
                            start,
                            result,
                            enabled_features,
                        )
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        let config = self.config.clone();
        let config_map_res_first = config.clone();

        ServiceBuilder::new()
            .instrument(move |req: &ExecutionRequest| {
                let operation_kind = req.query_plan.query.operation.kind();

                match operation_kind {
                    OperationKind::Subscription => info_span!(
                        EXECUTION_SPAN_NAME,
                        "otel.kind" = "INTERNAL",
                        "graphql.operation.type" = operation_kind.as_apollo_operation_type(),
                        "apollo_private.operation.subtype" =
                            OperationSubType::SubscriptionRequest.as_str(),
                    ),
                    _ => info_span!(
                        EXECUTION_SPAN_NAME,
                        "otel.kind" = "INTERNAL",
                        "graphql.operation.type" = operation_kind.as_apollo_operation_type(),
                    ),
                }
            })
            .and_then(move |resp: ExecutionResponse| {
                let config = config_map_res_first.clone();
                async move {
                    let resp = count_execution_errors(resp, &config.apollo.errors).await;
                    Ok::<_, BoxError>(resp)
                }
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let config = self.config.clone();
        let span_mode = self.config.instrumentation.spans.mode;
        let conf = self.config.clone();
        let subgraph_name = ByteString::from(name);
        let name = name.to_owned();
        let static_subgraph_instruments = self
            .builtin_instruments
            .read()
            .subgraph_custom_instruments
            .clone();
        let static_apollo_subgraph_instruments = self
            .builtin_instruments
            .read()
            .apollo_subgraph_instruments
            .clone();
        let static_cache_instruments = self
            .builtin_instruments
            .read()
            .cache_custom_instruments
            .clone();
        ServiceBuilder::new()
            .instrument(move |req: &SubgraphRequest| span_mode.create_subgraph(name.as_str(), req))
            .map_request(move |req: SubgraphRequest| request_ftv1(req))
            .map_response(move |resp| store_ftv1(&subgraph_name, resp))
            .map_future_with_request_data(
                move |sub_request: &SubgraphRequest| {
                    let custom_attributes = config
                        .instrumentation
                        .spans
                        .subgraph
                        .attributes
                        .on_request(sub_request);
                    let custom_instruments = config
                        .instrumentation
                        .instruments
                        .new_subgraph_instruments(static_subgraph_instruments.clone());
                    custom_instruments.on_request(sub_request);
                    let mut custom_events = config.instrumentation.events.new_subgraph_events();
                    custom_events.on_request(sub_request);

                    let apollo_instruments: ApolloSubgraphInstruments = config
                        .instrumentation
                        .instruments
                        .new_apollo_subgraph_instruments(
                            static_apollo_subgraph_instruments.clone(),
                            config.apollo.clone(),
                        );
                    apollo_instruments.on_request(sub_request);

                    let custom_cache_instruments: CacheInstruments = config
                        .instrumentation
                        .instruments
                        .new_cache_instruments(static_cache_instruments.clone());
                    custom_cache_instruments.on_request(sub_request);

                    (
                        sub_request.context.clone(),
                        custom_instruments,
                        custom_attributes,
                        custom_events,
                        apollo_instruments,
                        custom_cache_instruments,
                    )
                },
                move |(
                    context,
                    custom_instruments,
                    custom_attributes,
                    mut custom_events,
                    apollo_instruments,
                    custom_cache_instruments,
                ): (
                    Context,
                    SubgraphInstruments,
                    Vec<KeyValue>,
                    SubgraphEvents,
                    ApolloSubgraphInstruments,
                    CacheInstruments,
                ),
                      f: BoxFuture<'static, Result<SubgraphResponse, BoxError>>| {
                    let conf = conf.clone();
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
                                apollo_instruments.on_response(resp);
                                custom_cache_instruments.on_response(resp);
                                custom_instruments.on_response(resp);
                                custom_events.on_response(resp);
                            }
                            Err(err) => {
                                span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);

                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .subgraph
                                        .attributes
                                        .on_error(err, &context),
                                );
                                apollo_instruments.on_error(err, &context);
                                custom_cache_instruments.on_error(err, &context);
                                custom_instruments.on_error(err, &context);
                                custom_events.on_error(err, &context);
                            }
                        }

                        if let Ok(resp) = result {
                            Ok(count_subgraph_errors(resp, &conf.apollo.errors).await)
                        } else {
                            result
                        }
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn connector_request_service(
        &self,
        service: connector::request_service::BoxService,
        source_name: String,
    ) -> connector::request_service::BoxService {
        let req_fn_config = self.config.clone();
        let res_fn_config = self.config.clone();
        let span_mode = self.config.instrumentation.spans.mode;
        let static_connector_instruments = self
            .builtin_instruments
            .read()
            .connector_custom_instruments
            .clone();
        let static_apollo_connector_instruments = self
            .builtin_instruments
            .read()
            .apollo_connector_instruments
            .clone();
        ServiceBuilder::new()
            .instrument(move |_req: &connector::request_service::Request| {
                span_mode.create_connector(source_name.as_str())
            })
            .map_future_with_request_data(
                move |request: &connector::request_service::Request| {
                    let custom_instruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_connector_instruments(static_connector_instruments.clone());
                    custom_instruments.on_request(request);
                    let apollo_instruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_apollo_connector_instruments(
                            static_apollo_connector_instruments.clone(),
                            req_fn_config.apollo.clone(),
                        );
                    apollo_instruments.on_request(request);
                    let mut custom_events =
                        req_fn_config.instrumentation.events.new_connector_events();
                    custom_events.on_request(request);

                    let custom_span_attributes = req_fn_config
                        .instrumentation
                        .spans
                        .connector
                        .attributes
                        .on_request(request);

                    (
                        request.context.clone(),
                        custom_instruments,
                        apollo_instruments,
                        custom_events,
                        custom_span_attributes,
                    )
                },
                move |(
                    context,
                    custom_instruments,
                    apollo_connector_instruments,
                    mut custom_events,
                    custom_span_attributes,
                ): (
                    Context,
                    ConnectorInstruments,
                    ApolloConnectorInstruments,
                    ConnectorEvents,
                    Vec<KeyValue>,
                ),
                      f: BoxFuture<
                    'static,
                    Result<connector::request_service::Response, BoxError>,
                >| {
                    let conf = res_fn_config.clone();
                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_span_attributes);

                        let result = f.await;
                        match &result {
                            Ok(response) => {
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_response(response),
                                );
                                custom_instruments.on_response(response);
                                apollo_connector_instruments.on_response(response);
                                custom_events.on_response(response);
                            }
                            Err(err) => {
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_error(err, &context),
                                );
                                custom_instruments.on_error(err, &context);
                                apollo_connector_instruments.on_error(err, &context);
                                custom_events.on_error(err, &context);
                            }
                        }
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

    fn activate(&self) {
        let mut activation = self.activation.lock();
        if activation.is_active {
            return;
        }

        // Only apply things if we were executing in the context of a vanilla the Apollo executable.
        // Users that are rolling their own routers will need to set up telemetry themselves.
        if let Some(hot_tracer) = OPENTELEMETRY_TRACER_HANDLE.get() {
            // The reason that this has to happen here is that we are interacting with global state.
            // If we do this logic during plugin init then if a subsequent plugin fails to init then we
            // will already have set the new tracer provider and we will be in an inconsistent state.
            // activate is infallible, so if we get here we know the new pipeline is ready to go.
            let tracer_provider = activation
                .tracer_provider
                .take()
                .expect("must have new tracer_provider");

            let tracer = tracer_provider
                .tracer_builder(GLOBAL_TRACER_NAME)
                .with_version(env!("CARGO_PKG_VERSION"))
                .build();
            hot_tracer.reload(tracer);

            let last_provider = opentelemetry::global::set_tracer_provider(tracer_provider);

            Self::checked_global_tracer_shutdown(last_provider);

            let propagator = Self::create_propagator(&self.config);
            opentelemetry::global::set_text_map_propagator(propagator);
        }

        activation.reload_metrics();

        *self.builtin_instruments.write() =
            create_builtin_instruments(&self.config.instrumentation.instruments);
        reload_fmt(create_fmt_layer(&self.config));
        activation.is_active = true;
    }
}

impl Telemetry {
    fn create_propagator(config: &config::Conf) -> TextMapCompositePropagator {
        let propagation = &config.exporters.tracing.propagation;

        let tracing = &config.exporters.tracing;

        let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
        // TLDR the jaeger propagator MUST BE the first one because the version of opentelemetry_jaeger is buggy.
        // It overrides the current span context with an empty one if it doesn't find the corresponding headers.
        // Waiting for the >=0.16.1 release
        if propagation.jaeger {
            propagators.push(Box::<opentelemetry_jaeger_propagator::Propagator>::default());
        }
        if propagation.baggage {
            propagators.push(Box::<opentelemetry_sdk::propagation::BaggagePropagator>::default());
        }
        if Self::is_trace_context_propagation_active(config) {
            propagators
                .push(Box::<opentelemetry_sdk::propagation::TraceContextPropagator>::default());
        }
        if propagation.zipkin || tracing.zipkin.enabled {
            propagators.push(Box::<opentelemetry_zipkin::Propagator>::default());
        }
        if Self::is_datadog_propagation_active(config) {
            propagators.push(Box::<tracing::datadog_exporter::DatadogPropagator>::default());
        }
        if propagation.aws_xray {
            propagators.push(Box::<opentelemetry_aws::trace::XrayPropagator>::default());
        }

        // This propagator MUST come last because the user is trying to override the default behavior of the
        // other propagators.
        if let Some(from_request_header) = &propagation.request.header_name {
            propagators.push(Box::new(CustomTraceIdPropagator::new(
                from_request_header.to_string(),
                propagation.request.format.clone(),
            )));
        }

        TextMapCompositePropagator::new(propagators)
    }

    /// Check if Datadog propagation is active.
    /// This includes both explicit configuration and implicit activation via the Datadog exporter.
    fn is_datadog_propagation_active(config: &config::Conf) -> bool {
        let propagation = &config.exporters.tracing.propagation;
        let tracing = &config.exporters.tracing;
        propagation.datadog || tracing.datadog.enabled()
    }

    /// Check if trace context propagation is active.
    /// This includes both explicit configuration and implicit activation via the OTLP exporter.
    fn is_trace_context_propagation_active(config: &config::Conf) -> bool {
        let propagation = &config.exporters.tracing.propagation;
        let tracing = &config.exporters.tracing;
        propagation.trace_context || tracing.otlp.enabled
    }

    pub(crate) fn validate_propagation_compatibility(
        config: &config::Conf,
    ) -> Result<(), BoxError> {
        // Check if both Datadog and trace context propagation are active
        let datadog_active = Self::is_datadog_propagation_active(config);
        let trace_context_active = Self::is_trace_context_propagation_active(config);

        if datadog_active && trace_context_active {
            return Err(BoxError::from(
                "Configuration error: Datadog and `trace_context` propagation cannot be enabled at the same time due to incompatibilities in the trace ID formats. \
                 Please disable one of the following: \
                 - Set `telemetry.exporters.tracing.propagation.datadog: false` to disable Datadog propagation, or \
                 - Set `telemetry.exporters.tracing.propagation.trace_context: false` to disable trace context propagation.",
            ));
        }

        Ok(())
    }

    fn create_tracer_provider(
        config: &config::Conf,
    ) -> Result<opentelemetry_sdk::trace::TracerProvider, BoxError> {
        let tracing_config = &config.exporters.tracing;
        let spans_config = &config.instrumentation.spans;
        let common = &tracing_config.common;

        let mut builder =
            opentelemetry_sdk::trace::TracerProvider::builder().with_config((common).into());

        builder = setup_tracing(builder, &tracing_config.zipkin, common, spans_config)?;
        builder = setup_tracing(builder, &tracing_config.datadog, common, spans_config)?;
        builder = setup_tracing(builder, &tracing_config.otlp, common, spans_config)?;
        builder = setup_tracing(builder, &config.apollo, common, spans_config)?;

        let tracer_provider = builder.build();
        Ok(tracer_provider)
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
        custom_instruments: SupergraphInstruments,
        custom_events: SupergraphEvents,
        custom_graphql_instruments: GraphQLInstruments,
    ) -> Result<SupergraphResponse, BoxError> {
        let response = result?;
        let ctx = context.clone();
        // Wait for the first response of the stream
        let (parts, stream) = response.response.into_parts();
        let config_cloned = config.clone();
        let stream = stream.inspect(move |resp| {
            let span = Span::current();
            span.set_span_dyn_attributes(
                config_cloned
                    .instrumentation
                    .spans
                    .supergraph
                    .attributes
                    .on_response_event(resp, &ctx),
            );
            custom_instruments.on_response_event(resp, &ctx);
            custom_events.on_response_event(resp, &ctx);
            custom_graphql_instruments.on_response_event(resp, &ctx);
        });
        let (first_response, rest) = StreamExt::into_future(stream).await;

        let response = http::Response::from_parts(
            parts,
            once(ready(first_response.unwrap_or_default()))
                .chain(rest)
                .boxed(),
        );

        Ok(SupergraphResponse { context, response })
    }

    fn populate_context(field_level_instrumentation_ratio: f64, req: &SupergraphRequest) {
        let context = &req.context;

        // List of custom attributes for metrics
        let mut attributes: HashMap<String, AttributeValue> = HashMap::new();
        if let Some(operation_name) = &req.supergraph_request.body().operation_name {
            attributes.insert(
                OPERATION_NAME.to_string(),
                AttributeValue::String(operation_name.clone()),
            );
        }

        if rand::rng().random_bool(field_level_instrumentation_ratio) {
            context
                .extensions()
                .with_lock(|lock| lock.insert(EnableSubgraphFtv1));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_metrics_on_response_events(
        ctx: &Context,
        config: Arc<Conf>,
        field_level_instrumentation_ratio: f64,
        sender: Sender,
        start: Instant,
        result: Result<supergraph::Response, BoxError>,
        enabled_features: EnabledFeatures,
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
                        Default::default(),
                        enabled_features.clone(),
                    );
                }

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
                        Default::default(),
                        enabled_features.clone(),
                    );
                }
                Ok(router_response.map(move |response_stream| {
                    let sender = sender.clone();
                    let ctx = ctx.clone();

                    // Local field stats are recorded when enabled by the experimental configuration flag.
                    // For subscriptions, metrics are sent to Studio after each event, otherwise the metrics
                    // are sent to Studio after the last response. In the case of deferred responses, the last
                    // response needs to submit an aggregation of metrics across the primary and incremental
                    // responses. To avoid submitting duplicates, the recorder's contents are drained each time
                    // metrics are submitted.
                    let mut local_stat_recorder = LocalTypeStatRecorder::new();

                    response_stream
                        .enumerate()
                        .map(move |(idx, response)| {
                            let has_errors = !response.errors.is_empty();
                            if !matches!(sender, Sender::Noop) {
                                if let (true, Some(query)) = (
                                    config.apollo.experimental_local_field_metrics,
                                    ctx.executable_document(),
                                ) {
                                    local_stat_recorder.visit(
                                        &query,
                                        &response,
                                        &ctx.get_demand_control_context()
                                            .map(|c| c.variables)
                                            .unwrap_or_default(),
                                    );
                                }

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
                                                local_stat_recorder
                                                    .local_type_stats
                                                    .drain()
                                                    .collect(),
                                                enabled_features.clone(),
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
                                            local_stat_recorder.local_type_stats.drain().collect(),
                                            enabled_features.clone(),
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
                                            local_stat_recorder.local_type_stats.drain().collect(),
                                            enabled_features.clone(),
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

    #[allow(clippy::too_many_arguments)]
    fn update_apollo_metrics(
        context: &Context,
        field_level_instrumentation_ratio: f64,
        sender: Sender,
        has_errors: bool,
        duration: Duration,
        operation_kind: OperationKind,
        operation_subtype: Option<OperationSubType>,
        local_per_type_stat: HashMap<String, LocalTypeStat>,
        enabled_features: EnabledFeatures,
    ) {
        let metrics = if let Some(usage_reporting) = context
            .extensions()
            .with_lock(|lock| lock.get::<Arc<UsageReporting>>().cloned())
        {
            let licensed_operation_count = licensed_operation_count(&usage_reporting);
            let persisted_query_hit = context
                .get::<_, bool>(PERSISTED_QUERY_CACHE_HIT)
                .unwrap_or_default();

            if context
                .get(STUDIO_EXCLUDE)
                .is_ok_and(|x| x.unwrap_or_default())
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
                    router_features_enabled: enabled_features.list(),
                    ..Default::default()
                }
            } else {
                let traces = Self::subgraph_ftv1_traces(context);
                let per_type_stat = Self::per_type_stat(&traces, field_level_instrumentation_ratio);
                let root_error_stats = Self::per_path_error_stats(&traces);
                let strategy = context.get_demand_control_context().map(|c| c.strategy);
                let limits_stats = context.extensions().with_lock(|guard| {
                    let query_limits = guard.get::<OperationLimits<u32>>();
                    SingleLimitsStats {
                        strategy: strategy.and_then(|s| serde_json::to_string(&s.mode).ok()),
                        cost_estimated: context.get_estimated_cost().ok().flatten(),
                        cost_actual: context.get_actual_cost().ok().flatten(),

                        // These limits are related to the Traffic Shaping feature, unrelated to the Demand Control plugin
                        depth: query_limits.map_or(0, |ql| ql.depth as u64),
                        height: query_limits.map_or(0, |ql| ql.height as u64),
                        alias_count: query_limits.map_or(0, |ql| ql.aliases as u64),
                        root_field_count: query_limits.map_or(0, |ql| ql.root_fields as u64),
                    }
                });

                // If extended references or enums from responses are populated, we want to add them to the SingleStatsReport
                let extended_references = context
                    .extensions()
                    .with_lock(|lock| lock.get::<ExtendedReferenceStats>().cloned())
                    .unwrap_or_default();
                // Clear the enum values from responses when we send them in a report so that we properly report enum response
                // values for deferred responses and subscriptions.
                let enum_response_references = context
                    .extensions()
                    .with_lock(|lock| lock.remove::<ReferencedEnums>())
                    .unwrap_or_default();

                let maybe_pq_id = context
                    .extensions()
                    .with_lock(|lock| lock.get::<RequestPersistedQueryId>().cloned())
                    .map(|u| u.pq_id);
                let usage_reporting = if let Some(pq_id) = maybe_pq_id {
                    Arc::new(usage_reporting.with_pq_id(pq_id))
                } else {
                    usage_reporting
                };

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
                        usage_reporting.get_stats_report_key(),
                        SingleStats {
                            stats_with_context: SingleContextualizedStats {
                                context: StatsContext {
                                    result: "".to_string(),
                                    client_name: context
                                        .get(CLIENT_NAME)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                    client_version: context
                                        .get(CLIENT_VERSION)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                    client_library_name: context
                                        .get(CLIENT_LIBRARY_NAME)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                    client_library_version: context
                                        .get(CLIENT_LIBRARY_VERSION)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                    operation_type: operation_kind
                                        .as_apollo_operation_type()
                                        .to_string(),
                                    operation_subtype: operation_subtype
                                        .map(|op| op.to_string())
                                        .unwrap_or_default(),
                                },
                                limits_stats,
                                query_latency_stats: SingleQueryLatencyStats {
                                    latency: duration,
                                    has_errors,
                                    persisted_query_hit,
                                    root_error_stats,
                                    ..Default::default()
                                },
                                per_type_stat,
                                extended_references,
                                enum_response_references,
                                local_per_type_stat,
                            },
                            referenced_fields_by_type: usage_reporting
                                .get_referenced_fields()
                                .into_iter()
                                .map(|(k, v)| (k, convert(v)))
                                .collect(),
                            query_metadata: usage_reporting.get_query_metadata(),
                        },
                    )]),
                    router_features_enabled: enabled_features.list(),
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
                router_features_enabled: enabled_features.list(),
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
                    length: ListLengthHistogram::new(None),
                });
            let latency = Duration::from_nanos(node.end_time.saturating_sub(node.start_time));
            field_stat
                .latency
                .record(Some(latency), field_execution_weight);
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
        let mut attributes = Vec::new();
        if MetricsConfigurator::enabled(&config.exporters.metrics.otlp) {
            attributes.push(KeyValue::new("telemetry.metrics.otlp", true));
        }
        if config.exporters.metrics.prometheus.enabled {
            attributes.push(KeyValue::new("telemetry.metrics.prometheus", true));
        }
        if TracingConfigurator::enabled(&config.exporters.tracing.otlp) {
            attributes.push(KeyValue::new("telemetry.tracing.otlp", true));
        }
        if config.exporters.tracing.datadog.enabled() {
            attributes.push(KeyValue::new("telemetry.tracing.datadog", true));
        }
        if config.exporters.tracing.zipkin.enabled() {
            attributes.push(KeyValue::new("telemetry.tracing.zipkin", true));
        }

        if !attributes.is_empty() {
            u64_counter!(
                "apollo.router.operations.telemetry",
                "Telemetry exporters enabled",
                1,
                attributes
            );
        }
    }

    fn checked_tracer_shutdown(tracer_provider: opentelemetry_sdk::trace::TracerProvider) {
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

    fn extract_enabled_features(full_config: &serde_json::Value) -> EnabledFeatures {
        EnabledFeatures {
            // The APQ cache enabled config defaults to true.
            // The distributed APQ cache is only considered enabled if the redis config is also set.
            distributed_apq_cache: {
                let enabled = full_config["apq"]["enabled"].as_bool().unwrap_or(true);
                let redis_cache_config_set =
                    full_config["apq"]["router"]["cache"]["redis"].is_object();
                enabled && redis_cache_config_set
            },
            // Entity cache's top-level enabled flag defaults to false. If the top-level flag is
            // enabled, the feature is considered enabled regardless of the subgraph-level enabled
            // settings.
            entity_cache: full_config["preview_entity_cache"]["enabled"]
                .as_bool()
                .unwrap_or(false),
        }
    }
}

impl TelemetryActivation {
    fn reload_metrics(&mut self) {
        let meter_provider = meter_provider_internal();
        commit_prometheus();
        let mut old_meter_providers: [Option<FilterMeterProvider>; 4] = Default::default();

        old_meter_providers[0] = meter_provider.set(
            MeterProviderType::PublicPrometheus,
            self.public_prometheus_meter_provider.take(),
        );

        old_meter_providers[1] = meter_provider.set(
            MeterProviderType::Apollo,
            self.private_meter_provider.take(),
        );

        old_meter_providers[2] = meter_provider.set(
            MeterProviderType::ApolloRealtime,
            self.private_realtime_meter_provider.take(),
        );

        old_meter_providers[3] =
            meter_provider.set(MeterProviderType::Public, self.public_meter_provider.take());

        Self::checked_meter_shutdown(old_meter_providers);
    }

    fn checked_meter_shutdown(meters: [Option<FilterMeterProvider>; 4]) {
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

fn licensed_operation_count(usage_reporting: &UsageReporting) -> u64 {
    match usage_reporting {
        UsageReporting::Error(_) => 0,
        _ => 1,
    }
}

fn convert(
    referenced_fields: crate::apollo_studio_interop::ReferencedFieldsForType,
) -> crate::plugins::telemetry::apollo_exporter::proto::reports::ReferencedFieldsForType {
    crate::plugins::telemetry::apollo_exporter::proto::reports::ReferencedFieldsForType {
        field_names: referenced_fields.field_names,
        is_interface: referenced_fields.is_interface,
    }
}

register_private_plugin!("apollo", "telemetry", Telemetry);

fn request_ftv1(mut req: SubgraphRequest) -> SubgraphRequest {
    if req
        .context
        .extensions()
        .with_lock(|lock| lock.contains_key::<EnableSubgraphFtv1>())
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
        .with_lock(|lock| lock.contains_key::<EnableSubgraphFtv1>())
        && let Some(serde_json_bytes::Value::String(ftv1)) =
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
    resp
}

/// CustomTraceIdPropagator to set custom trace_id for our tracing system
/// coming from headers
#[derive(Debug)]
struct CustomTraceIdPropagator {
    header_name: String,
    fields: [String; 1],
    format: TraceIdFormat,
}

impl CustomTraceIdPropagator {
    fn new(header_name: String, format: TraceIdFormat) -> Self {
        Self {
            fields: [header_name.clone()],
            header_name,
            format,
        }
    }

    fn extract_span_context(&self, extractor: &dyn Extractor) -> Option<SpanContext> {
        let trace_id = extractor.get(&self.header_name)?;
        let trace_id = trace_id.replace('-', "");

        // extract trace id
        let trace_id = match opentelemetry::trace::TraceId::from_hex(&trace_id) {
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
        if span_context.trace_id() != TraceId::INVALID {
            let formatted_trace_id = self.format.format(span_context.trace_id());
            injector.set(&self.header_name, formatted_trace_id);
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

pub(crate) fn add_query_attributes(context: &Context, custom_attributes: &mut Vec<KeyValue>) {
    context.extensions().with_lock(|c| {
        if let Some(limits) = c.get::<OperationLimits<u32>>() {
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_ALIASES.clone(),
                AttributeValue::I64(limits.aliases.into()),
            ));
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_DEPTH.clone(),
                AttributeValue::I64(limits.depth.into()),
            ));
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_HEIGHT.clone(),
                AttributeValue::I64(limits.height.into()),
            ));
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_ROOT_FIELDS.clone(),
                AttributeValue::I64(limits.root_fields.into()),
            ));
        }
    });
}

struct EnableSubgraphFtv1;

//
// Please ensure that any tests added to the tests module use the tokio multi-threaded test executor.
//
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use axum_extra::headers::HeaderName;
    use http::HeaderMap;
    use http::HeaderValue;
    use http::StatusCode;
    use http::header::CONTENT_TYPE;
    use insta::assert_snapshot;
    use itertools::Itertools;
    use opentelemetry::propagation::Injector;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use serde_json::Value;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::json;
    use tower::Service;
    use tower::ServiceExt;
    use tower::util::BoxService;

    use super::CustomTraceIdPropagator;
    use super::EnabledFeatures;
    use super::Telemetry;
    use super::apollo::ForwardHeaders;
    use super::config::Conf;
    use crate::error::FetchError;
    use crate::graphql;
    use crate::graphql::Error;
    use crate::graphql::IntoGraphQLErrors;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::json_ext::Object;
    use crate::metrics::FutureMetricsExt;
    use crate::plugin::DynPlugin;
    use crate::plugin::PluginInit;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugins::demand_control::COST_ACTUAL_KEY;
    use crate::plugins::demand_control::COST_ESTIMATED_KEY;
    use crate::plugins::demand_control::COST_RESULT_KEY;
    use crate::plugins::demand_control::COST_STRATEGY_KEY;
    use crate::plugins::demand_control::DemandControlError;
    use crate::plugins::telemetry::EnableSubgraphFtv1;
    use crate::plugins::telemetry::config::TraceIdFormat;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::SupergraphRequest;
    use crate::services::SupergraphResponse;
    use crate::services::router;

    macro_rules! assert_prometheus_metrics {
        ($plugin:expr) => {{
            let prometheus_metrics = get_prometheus_metrics($plugin.as_ref()).await;
            let regexp = regex::Regex::new(
                r#"process_executable_name="(?P<process>[^"]+)",?|service_name="(?P<service>[^"]+)",?"#,
            )
            .unwrap();
            let prometheus_metrics = regexp.replace_all(&prometheus_metrics, "").to_owned();
            assert_snapshot!(prometheus_metrics.replace(
                &format!(r#"service_version="{}""#, std::env!("CARGO_PKG_VERSION")),
                r#"service_version="X""#
            ));
        }};
    }

    async fn create_plugin_with_config(full_config: &str) -> Box<dyn DynPlugin> {
        let full_config = serde_yaml::from_str::<Value>(full_config).expect("yaml must be valid");
        let telemetry_config = full_config
            .as_object()
            .expect("must be an object")
            .get("telemetry")
            .expect("telemetry must be a root key");
        let init = PluginInit::fake_builder()
            .config(telemetry_config.clone())
            .full_config(full_config)
            .build()
            .with_deserialized_config()
            .expect("unable to deserialize telemetry config");

        let plugin = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(init)
            .await
            .expect("unable to create telemetry plugin");

        let downcast = plugin
            .as_any()
            .downcast_ref::<Telemetry>()
            .expect("Telemetry plugin expected");
        if downcast.config.exporters.metrics.prometheus.enabled {
            downcast.activation.lock().reload_metrics();
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
            .body(axum::body::Body::empty())
            .unwrap();
        let mut resp = web_endpoint.oneshot(http_req_prom).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = router::body::into_bytes(resp.body_mut()).await.unwrap();
        String::from_utf8_lossy(&body)
            .split('\n')
            .filter(|l| l.contains("bucket"))
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
        let full_config = serde_json::json!({
            "telemetry": {
                "apollo": {
                    "schema_id": "abc"
                },
                "exporters": {
                    "tracing": {},
                },
            },
        });
        let telemetry_config = full_config["telemetry"].clone();
        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(
                PluginInit::fake_builder()
                    .config(telemetry_config)
                    .full_config(full_config)
                    .build(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn config_serialization() {
        create_plugin_with_config(include_str!("testdata/config.router.yaml")).await;
    }

    #[tokio::test]
    async fn test_enabled_features() {
        // Explicitly enabled
        let plugin = create_plugin_with_config(include_str!(
            "testdata/full_config_all_features_enabled.router.yaml"
        ))
        .await;
        let features = enabled_features(plugin.as_ref());
        assert!(
            features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature enabled when explicitly enabled"
        );
        assert!(
            features.entity_cache,
            "Telemetry plugin should consider entity cache feature enabled when explicitly enabled"
        );

        // Explicitly disabled
        let plugin = create_plugin_with_config(include_str!(
            "testdata/full_config_all_features_explicitly_disabled.router.yaml"
        ))
        .await;
        let features = enabled_features(plugin.as_ref());
        assert!(
            !features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature disabled when explicitly disabled"
        );
        assert!(
            !features.entity_cache,
            "Telemetry plugin should consider entity cache feature disabled when explicitly disabled"
        );

        // Default Values
        let plugin = create_plugin_with_config(include_str!(
            "testdata/full_config_all_features_defaults.router.yaml"
        ))
        .await;
        let features = enabled_features(plugin.as_ref());
        assert!(
            !features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature disabled when all values are defaulted"
        );
        assert!(
            !features.entity_cache,
            "Telemetry plugin should consider entity cache feature disabled when all values are defaulted"
        );

        // APQ enabled when default enabled with redis config defined
        let plugin = create_plugin_with_config(include_str!(
            "testdata/full_config_apq_enabled_partial_defaults.router.yaml"
        ))
        .await;
        let features = enabled_features(plugin.as_ref());
        assert!(
            features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature enabled when top-level enabled flag is defaulted and redis config is defined"
        );

        // APQ disabled when default enabled with redis config NOT defined
        let plugin = create_plugin_with_config(include_str!(
            "testdata/full_config_apq_disabled_partial_defaults.router.yaml"
        ))
        .await;
        let features = enabled_features(plugin.as_ref());
        assert!(
            !features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature disabled when redis cache is not enabled"
        );
    }

    fn enabled_features(plugin: &dyn DynPlugin) -> &EnabledFeatures {
        &plugin
            .as_any()
            .downcast_ref::<Telemetry>()
            .expect("telemetry plugin")
            .enabled_features
    }

    #[tokio::test]
    async fn test_supergraph_metrics_ok() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml"))
                    .await;
            make_supergraph_request(plugin.as_ref()).await;

            assert_counter!(
                "http.request",
                1,
                "another_test" = "my_default_value",
                "my_value" = 2,
                "myname" = "label_value",
                "renamed_value" = "my_value_set",
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
                        .errors(vec![
                            crate::graphql::Error::builder()
                                .message("nope")
                                .extension_code("NOPE")
                                .build(),
                        ])
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
                "http.request",
                1,
                "another_test" = "my_default_value",
                "error" = "nope",
                "myname" = "label_value",
                "renamed_value" = "my_value_set"
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
            let plugin = Box::new(
                create_plugin_with_config(include_str!("testdata/custom_instruments.router.yaml"))
                    .await,
            );

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
    async fn test_field_instrumentation_sampler_with_preview_datadog_agent_sampling() {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/config.field_instrumentation_sampler.router.yaml"
        ))
        .await;

        let ftv1_counter = Arc::new(AtomicUsize::new(0));
        let ftv1_counter_cloned = ftv1_counter.clone();

        let mut mock_request_service = MockSupergraphService::new();
        mock_request_service
            .expect_call()
            .times(10)
            .returning(move |req: SupergraphRequest| {
                if req
                    .context
                    .extensions()
                    .with_lock(|lock| lock.contains_key::<EnableSubgraphFtv1>())
                {
                    ftv1_counter_cloned.fetch_add(1, Ordering::Relaxed);
                }
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::OK)
                    .header("content-type", "application/json")
                    .data(json!({"errors": [{"message": "nope"}]}))
                    .build()
                    .unwrap())
            });
        let mut request_supergraph_service =
            plugin.supergraph_service(BoxService::new(mock_request_service));

        for _ in 0..10 {
            let supergraph_req = SupergraphRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());
            let _router_response = request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
        }
        // It should be 100% because when we set preview_datadog_agent_sampling, we only take the value of field_level_instrumentation_sampler
        assert_eq!(ftv1_counter.load(Ordering::Relaxed), 10);
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
                .subgraph_name("my_subgraph_name")
                .build();
            let _subgraph_response = subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();

            assert_histogram_count!(
                "http.client.request.duration",
                1,
                "error" = "custom_error_for_propagation",
                "my_key" = "my_custom_attribute_from_context",
                "query_from_request" = "query { test }",
                "status" = 200,
                "subgraph" = "my_subgraph_name",
                "subgraph_error_extended_code" = "FETCH_ERROR"
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
                .subgraph_name("my_subgraph_name_error")
                .build();
            let _subgraph_response = subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .expect_err("should be an error");

            assert_histogram_count!(
                "http.client.request.duration",
                1,
                "message" =
                    "HTTP fetch failed from 'my_subgraph_name_error': cannot contact the subgraph",
                "subgraph" = "my_subgraph_name_error",
                "query_from_request" = "query { test }"
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
                .body(crate::services::router::body::empty())
                .unwrap();

            let resp = <axum::Router as tower::ServiceExt<http::Request<axum::body::Body>>>::ready(
                &mut web_endpoint,
            )
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
            u64_histogram!("apollo.test.histo", "it's a test", 1u64);

            make_supergraph_request(plugin.as_ref()).await;
            assert_prometheus_metrics!(plugin);
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
            u64_histogram!("apollo.test.histo", "it's a test", 1u64);

            make_supergraph_request(plugin.as_ref()).await;
            assert_prometheus_metrics!(plugin);
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
            u64_histogram!("apollo.test.histo", "it's a test", 1u64);
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_custom_view_drop() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/prometheus_custom_view_drop.router.yaml"
            ))
            .await;
            make_supergraph_request(plugin.as_ref()).await;
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_units_are_included() {
        async {
            let plugin =
                create_plugin_with_config(include_str!("testdata/prometheus.router.yaml")).await;
            u64_histogram_with_unit!("apollo.test.histo1", "no unit", "{request}", 1u64);
            f64_histogram_with_unit!("apollo.test.histo2", "unit", "s", 1f64);

            make_supergraph_request(plugin.as_ref()).await;
            assert_prometheus_metrics!(plugin);
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
    async fn test_custom_trace_id_propagator_strip_dashes_in_trace_id() {
        let header = String::from("x-trace-id");
        let trace_id = String::from("04f9e396-465c-4840-bc2b-f493b8b1a7fc");
        let expected_trace_id = String::from("04f9e396465c4840bc2bf493b8b1a7fc");

        let propagator = CustomTraceIdPropagator::new(header.clone(), TraceIdFormat::Uuid);
        let mut headers: HashMap<String, String> = HashMap::new();
        headers.insert(header, trace_id);
        let span = propagator.extract_span_context(&headers);
        assert!(span.is_some());
        assert_eq!(span.unwrap().trace_id().to_string(), expected_trace_id);
    }

    #[test]
    fn test_header_propagation_format() {
        struct Injected(HashMap<String, String>);
        impl Injector for Injected {
            fn set(&mut self, key: &str, value: String) {
                self.0.insert(key.to_string(), value);
            }
        }
        let mut injected = Injected(HashMap::new());
        let _ctx = opentelemetry::Context::new()
            .with_remote_span_context(SpanContext::new(
                TraceId::from_u128(0x04f9e396465c4840bc2bf493b8b1a7fc),
                SpanId::INVALID,
                TraceFlags::default(),
                false,
                TraceState::default(),
            ))
            .attach();
        let propagator = CustomTraceIdPropagator::new("my_header".to_string(), TraceIdFormat::Uuid);
        propagator.inject_context(&opentelemetry::Context::current(), &mut injected);
        assert_eq!(
            injected.0.get("my_header").unwrap(),
            "04f9e396-465c-4840-bc2b-f493b8b1a7fc"
        );
    }

    #[derive(Clone)]
    struct CostContext {
        pub(crate) estimated: f64,
        pub(crate) actual: f64,
        pub(crate) result: &'static str,
        pub(crate) strategy: &'static str,
    }

    async fn make_failed_demand_control_request(plugin: &dyn DynPlugin, cost_details: CostContext) {
        let mut mock_service = MockSupergraphService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                req.context.extensions().with_lock(|lock| {
                    lock.insert(cost_details.clone());
                });
                req.context
                    .insert(COST_ESTIMATED_KEY, cost_details.estimated)
                    .unwrap();
                req.context
                    .insert(COST_ACTUAL_KEY, cost_details.actual)
                    .unwrap();
                req.context
                    .insert(COST_RESULT_KEY, cost_details.result.to_string())
                    .unwrap();
                req.context
                    .insert(COST_STRATEGY_KEY, cost_details.strategy.to_string())
                    .unwrap();

                let errors = if cost_details.result == "COST_ESTIMATED_TOO_EXPENSIVE" {
                    DemandControlError::EstimatedCostTooExpensive {
                        estimated_cost: cost_details.estimated,
                        max_cost: (cost_details.estimated - 5.0).max(0.0),
                    }
                    .into_graphql_errors()
                    .unwrap()
                } else if cost_details.result == "COST_ACTUAL_TOO_EXPENSIVE" {
                    DemandControlError::ActualCostTooExpensive {
                        actual_cost: cost_details.actual,
                        max_cost: (cost_details.actual - 5.0).max(0.0),
                    }
                    .into_graphql_errors()
                    .unwrap()
                } else {
                    Vec::new()
                };

                SupergraphResponse::fake_builder()
                    .context(req.context)
                    .data(
                        serde_json::to_value(graphql::Response::builder().errors(errors).build())
                            .unwrap(),
                    )
                    .build()
            });

        let mut service = plugin.supergraph_service(BoxService::new(mock_service));
        let router_req = SupergraphRequest::fake_builder().build().unwrap();
        let _router_response = service
            .ready()
            .await
            .unwrap()
            .call(router_req)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_demand_control_delta_filter() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/demand_control_delta_filter.router.yaml"
            ))
            .await;
            make_failed_demand_control_request(
                plugin.as_ref(),
                CostContext {
                    estimated: 10.0,
                    actual: 8.0,
                    result: "COST_ACTUAL_TOO_EXPENSIVE",
                    strategy: "static_estimated",
                },
            )
            .await;

            assert_histogram_sum!("cost.rejected.operations", 8.0);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_demand_control_result_filter() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/demand_control_result_filter.router.yaml"
            ))
            .await;
            make_failed_demand_control_request(
                plugin.as_ref(),
                CostContext {
                    estimated: 10.0,
                    actual: 0.0,
                    result: "COST_ESTIMATED_TOO_EXPENSIVE",
                    strategy: "static_estimated",
                },
            )
            .await;

            assert_histogram_sum!("cost.rejected.operations", 10.0);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_demand_control_result_attributes() {
        async {
            let plugin = create_plugin_with_config(include_str!(
                "testdata/demand_control_result_attribute.router.yaml"
            ))
            .await;
            make_failed_demand_control_request(
                plugin.as_ref(),
                CostContext {
                    estimated: 10.0,
                    actual: 0.0,
                    result: "COST_ESTIMATED_TOO_EXPENSIVE",
                    strategy: "static_estimated",
                },
            )
            .await;

            assert_histogram_sum!(
                "cost.estimated",
                10.0,
                "cost.result" = "COST_ESTIMATED_TOO_EXPENSIVE"
            );
        }
        .with_metrics()
        .await;
    }

    #[test]
    fn test_validate_propagation_compatibility_both_datadog_and_trace_context_enabled() {
        // Test using YAML configuration to set the enabled field properly
        let config_yaml = r#"
exporters:
  tracing:
    datadog:
      enabled: true
    propagation:
      datadog: true
      trace_context: true
"#;
        let config: Conf = serde_yaml::from_str(config_yaml).expect("Valid config");

        let result = Telemetry::validate_propagation_compatibility(&config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Datadog"));
        assert!(err.to_string().contains("`trace_context` propagation"));
        assert!(
            err.to_string()
                .contains("incompatibilities in the trace ID formats")
        );
    }

    #[test]
    fn test_validate_propagation_compatibility_datadog_enabled_no_trace_context() {
        let config_yaml = r#"
exporters:
  tracing:
    datadog:
      enabled: true
    propagation:
      datadog: true
      trace_context: false
"#;
        let config: Conf = serde_yaml::from_str(config_yaml).expect("Valid config");

        let result = Telemetry::validate_propagation_compatibility(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_propagation_compatibility_trace_context_enabled_no_datadog() {
        let config_yaml = r#"
exporters:
  tracing:
    datadog:
      enabled: false
    propagation:
      datadog: false
      trace_context: true
"#;
        let config: Conf = serde_yaml::from_str(config_yaml).expect("Valid config");

        let result = Telemetry::validate_propagation_compatibility(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_propagation_compatibility_neither_enabled() {
        let config_yaml = r#"
exporters:
  tracing:
    datadog:
      enabled: false
    propagation:
      datadog: false
      trace_context: false
"#;
        let config: Conf = serde_yaml::from_str(config_yaml).expect("Valid config");

        let result = Telemetry::validate_propagation_compatibility(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_propagation_compatibility_datadog_implicit_via_exporter() {
        // Test case where datadog propagation is implicitly enabled via the datadog exporter
        // but trace_context is explicitly enabled
        let config_yaml = r#"
exporters:
  tracing:
    datadog:
      enabled: true
    propagation:
      datadog: false
      trace_context: true
"#;
        let config: Conf = serde_yaml::from_str(config_yaml).expect("Valid config");

        let result = Telemetry::validate_propagation_compatibility(&config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Datadog"));
        assert!(err.to_string().contains("trace context propagation"));
    }

    #[test]
    fn test_validate_propagation_compatibility_otlp_enabled_with_datadog() {
        // Test case where OTLP exporter (which enables trace context) is enabled with Datadog
        let config_yaml = r#"
exporters:
  tracing:
    datadog:
      enabled: true
    otlp:
      enabled: true
    propagation:
      datadog: true
      trace_context: false
"#;
        let config: Conf = serde_yaml::from_str(config_yaml).expect("Valid config");

        let result = Telemetry::validate_propagation_compatibility(&config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Datadog"));
        assert!(err.to_string().contains("trace context propagation"));
    }
}
