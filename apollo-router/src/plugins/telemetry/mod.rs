//! Telemetry plugin.
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use ::tracing::Span;
use config_new::instruments::InstrumentsConfig;
use config_new::instruments::StaticInstrument;
use http::HeaderName;
use metrics::apollo::studio::SingleLimitsStats;
use multimap::MultiMap;
use opentelemetry::Key;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::propagation::Extractor;
use opentelemetry::propagation::Injector;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::propagation::text_map_propagator::FieldIter;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceId;
use opentelemetry::trace::TraceState;
use parking_lot::Mutex;
use parking_lot::RwLock;
use regex::Regex;
use reload::activation::Activation;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tower::BoxError;
use uuid::Uuid;

use self::apollo::ForwardValues;
use self::apollo::LicensedOperationCountByType;
use self::apollo::OperationSubType;
use self::apollo_exporter::Sender;
use self::apollo_exporter::proto;
use self::config::TraceIdFormat;
use self::config_new::instruments::Instrumented;
use self::metrics::apollo::studio::SingleTypeStat;
use crate::Context;
use crate::ListenAddr;
use crate::apollo_studio_interop::ExtendedReferenceStats;
use crate::apollo_studio_interop::ReferencedEnums;
use crate::apollo_studio_interop::UsageReporting;
use crate::axum_factory::Endpoint;
use crate::metrics::meter_provider;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::limits::operation_limits::OperationLimits;
use crate::plugins::telemetry::apollo_exporter::proto::reports::StatsContext;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::node::Id::ResponseName;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::metrics::apollo::histogram::ListLengthHistogram;
use crate::plugins::telemetry::metrics::apollo::studio::LocalTypeStat;
use crate::plugins::telemetry::metrics::apollo::studio::SingleContextualizedStats;
use crate::plugins::telemetry::metrics::apollo::studio::SinglePathErrorStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleQueryLatencyStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStatsReport;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::tracing::apollo_telemetry::decode_ftv1_trace;
use crate::query_planner::OperationKind;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;
use crate::services::layers::apq::PERSISTED_QUERY_CACHE_HIT;
use crate::services::layers::persisted_queries::RequestPersistedQueryId;

pub(crate) mod apollo;
pub(crate) mod apollo_exporter;
pub(crate) mod apollo_otlp_exporter;
pub(crate) mod config;
pub(crate) mod config_new;
pub(crate) mod consts;
pub(crate) mod dynamic_attribute;
mod endpoint;
mod error_counter;
mod fmt_layer;
pub(crate) mod formatters;
mod layers;
mod logging;
pub(crate) mod metrics;
/// Opentelemetry utils
pub(crate) mod otel;
mod otlp;
pub(crate) mod pipeline_bypass;
pub(crate) mod reload;
pub(crate) mod resource;
pub(crate) mod span_ext;
pub(crate) mod span_factory;
pub(crate) mod tracing;
pub(crate) mod utils;

// Tracing consts
pub(crate) const CLIENT_NAME: &str = "apollo::telemetry::client_name";
pub(crate) const CLIENT_LIBRARY_NAME: &str = "apollo::telemetry::client_library_name";
pub(crate) const CLIENT_VERSION: &str = "apollo::telemetry::client_version";
pub(crate) const CLIENT_LIBRARY_VERSION: &str = "apollo::telemetry::client_library_version";
pub(crate) const SUBGRAPH_FTV1: &str = "apollo::telemetry::subgraph_ftv1";
pub(crate) const STUDIO_EXCLUDE: &str = "apollo::telemetry::studio_exclude";
const GLOBAL_TRACER_NAME: &str = "apollo-router";
const DEFAULT_EXPOSE_TRACE_ID_HEADER: &str = "apollo-trace-id";
static DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME: HeaderName =
    HeaderName::from_static(DEFAULT_EXPOSE_TRACE_ID_HEADER);

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
    activation: Mutex<Option<Activation>>,
    enabled_features: EnabledFeatures,
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
            .build();

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
}

/// Whether Apollo reporting is on, through a `telemetry.apollo` section or an Apollo key and graph
/// ref (`has_credentials`).
fn reports_to_apollo(full_config: &serde_json::Value, has_credentials: bool) -> bool {
    has_credentials
        || full_config
            .pointer("/telemetry/apollo")
            .is_some_and(serde_json::Value::is_object)
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
    }
}

#[derive(Clone, Debug)]
struct EnabledFeatures {
    distributed_apq_cache: bool,
    response_cache: bool,
}

impl EnabledFeatures {
    fn list(&self) -> Vec<String> {
        // Map enabled features to their names for usage reports
        [
            ("distributed_apq_cache", self.distributed_apq_cache),
            ("response_cache", self.response_cache),
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

        // Set up feature usage list
        let full_config = init
            .full_config
            .as_ref()
            .expect("Required full router configuration not found in telemetry plugin");

        let mut config = init.config;
        // Apollo reporting identifies the schema it reports against.
        if reports_to_apollo(
            full_config,
            apollo_key().is_some() && apollo_graph_reference().is_some(),
        ) {
            config.apollo.schema_id = init.supergraph_schema_id.to_string();
        }
        config.instrumentation.spans.update_defaults();
        config.instrumentation.instruments.update_defaults();
        if let Err(err) = config.instrumentation.validate() {
            ::tracing::warn!(
                "Potential configuration error for 'instrumentation': {err}, please check the documentation on https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/events"
            );
        }

        config.validate_per_exporter_samplers()?;
        let field_level_instrumentation_ratio =
            config.calculate_field_level_instrumentation_ratio()?;

        let (activation, custom_endpoints, apollo_metrics_sender) =
            reload::prepare(&init.previous_config, &config)?;

        let enabled_features = Self::extract_enabled_features(full_config);
        ::tracing::debug!("Enabled scale features: {:?}", enabled_features);

        Ok(Telemetry {
            custom_endpoints,
            apollo_metrics_sender,
            supergraph_schema_id: init.supergraph_schema_id,
            field_level_instrumentation_ratio,
            activation: Mutex::new(Some(activation)),
            builtin_instruments: RwLock::new(create_builtin_instruments(
                &config.instrumentation.instruments,
            )),
            enabled_features,
            config: Arc::new(config),
        })
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        self.custom_endpoints.clone()
    }

    fn activate(&self) {
        // activation called multiple times during startup due to telemetry needed to be initialized before
        // plugins are initialized
        if let Some(activation) = self.activation.lock().take() {
            activation.commit();
            // The reason this exist here is that these instruments use the global meter provider when created.
            // In future, we should directly use the meter provider from activation rather than the global
            // meter provider, this will eliminate the brittle sequencing of instrument creation.
            *self.builtin_instruments.write() =
                create_builtin_instruments(&self.config.instrumentation.instruments);
        }
    }
}

impl Telemetry {
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
        sender.send(metrics);
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
            // Response cache's top-level enabled flag defaults to false. If the top-level flag is
            // enabled, the feature is considered enabled regardless of the subgraph-level enabled
            // settings.
            response_cache: full_config["response_cache"]["enabled"]
                .as_bool()
                .unwrap_or(false),
        }
    }
}

// Regex for allowed values for client library names and versions
static VALID_CLIENT_LIBRARY_VALUE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ a-zA-Z0-9.@/_\-]{1,60}$").unwrap());

pub(crate) fn is_valid_client_library_value(value: &str) -> bool {
    VALID_CLIENT_LIBRARY_VALUE_REGEX.is_match(value)
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
                ::tracing::error!(trace_id = %trace_id, error = %err, "cannot generate custom trace_id");
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
        match self.extract_span_context(extractor) {
            Some(span_context) => cx.with_remote_span_context(span_context),
            None => cx.clone(),
        }
    }

    fn fields(&self) -> FieldIter<'_> {
        FieldIter::new(self.fields.as_ref())
    }
}

struct EnableSubgraphFtv1;

//
// Please ensure that any tests added to the tests module use the tokio multi-threaded test executor.
//
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use http::StatusCode;
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
    use serde_json_bytes::json;
    use tower::Service;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use super::CustomTraceIdPropagator;
    use super::Telemetry;
    use crate::graphql;
    use crate::graphql::IntoGraphQLErrors;
    use crate::metrics::FutureMetricsExt;
    use crate::plugin::DynPlugin;
    use crate::plugin::PluginInit;
    use crate::plugins::demand_control::COST_ACTUAL_KEY;
    use crate::plugins::demand_control::COST_ESTIMATED_KEY;
    use crate::plugins::demand_control::COST_RESULT_KEY;
    use crate::plugins::demand_control::COST_STRATEGY_KEY;
    use crate::plugins::demand_control::DemandControlError;
    use crate::plugins::telemetry::config::TraceIdFormat;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::SupergraphRequest;
    use crate::services::SupergraphResponse;
    use crate::services::router;

    // Serializes tests that call `plugin.activate()`. `Telemetry::activate()`
    // -> `Activation::commit()` performs two process-wide writes:
    //   1. `opentelemetry::global::set_tracer_provider(...)`
    //   2. `*REGISTRY.lock() = self.prometheus_registry.clone();`
    //      (the global Prometheus registry pointer in reload/activation.rs)
    // Neither is covered by `FutureMetricsExt::with_metrics`, which only
    // isolates the meter provider via a tokio task-local. When multiple
    // `it_test_prometheus_*` tests run in parallel under nextest, one test's
    // activate() can clobber another's global state mid-test, causing rare
    // but observable flakes against the per-plugin Prometheus registry scrape.
    //
    // See `src/plugins/telemetry/metrics/apollo/mod.rs` for the same pattern
    // applied to the apollo_metrics tests.
    //
    // Under `cargo nextest`, this set of tests is also serialized by the
    // `serial-prometheus-telemetry-unit` test-group in `.config/nextest.toml`.
    // The in-source mutex below is kept so that contributors running plain
    // `cargo test -p apollo-router` (which does not honour nextest config)
    // still get the serialization they need.
    static TEST: once_cell::sync::Lazy<Arc<tokio::sync::Mutex<()>>> =
        once_cell::sync::Lazy::new(Default::default);

    // TODO(@goto-bus-stop): this could perhaps use insta's redaction features instead?
    macro_rules! assert_prometheus_metrics {
        ($plugin:expr) => {{
            let prometheus_metrics = get_prometheus_metrics(&$plugin).await;
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

    async fn get_prometheus_metrics(plugin: &Telemetry) -> String {
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

    async fn make_supergraph_request(plugin: &Telemetry) {
        let (mock_service, mut handle) =
            tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                SupergraphResponse::fake_builder()
                    .context(req.context)
                    .header("x-custom", "coming_from_header")
                    .data(json!({"data": {"my_value": 2usize}}))
                    .build()
                    .unwrap(),
            );
        });
        let mut supergraph_service = ServiceBuilder::new()
            .layer(plugin.instrument_supergraph_layer())
            .service(mock_service);
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
        crate::plugin::test::await_mock_driver(driver).await;
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
            .with_metrics()
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_serialization() {
        PluginTestHarness::<Telemetry>::builder()
            .config(include_str!("testdata/config.router.yaml"))
            .build()
            .with_metrics()
            .await
            .expect("test harness");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_enabled_features() {
        // Explicitly enabled except response caching because entity caching and response caching are mutually exclusive
        let plugin = PluginTestHarness::<Telemetry>::builder()
            .config(include_str!(
                "testdata/full_config_all_features_enabled.router.yaml"
            ))
            .build()
            .with_metrics()
            .await
            .expect("test harness");
        let features = &plugin.enabled_features;
        assert!(
            features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature enabled when explicitly enabled"
        );

        // Explicitly enabled
        let plugin = PluginTestHarness::<Telemetry>::builder()
            .config(include_str!(
                "testdata/full_config_all_features_enabled_response_cache.router.yaml"
            ))
            .build()
            .with_metrics()
            .await
            .expect("test harness");
        let features = &plugin.enabled_features;
        assert!(
            features.response_cache,
            "Telemetry plugin should consider response cache feature enabled when explicitly enabled"
        );
        assert!(
            features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature enabled when explicitly enabled"
        );

        // Explicitly disabled
        let plugin = PluginTestHarness::<Telemetry>::builder()
            .config(include_str!(
                "testdata/full_config_all_features_explicitly_disabled.router.yaml"
            ))
            .build()
            .with_metrics()
            .await
            .expect("test harness");
        let features = &plugin.enabled_features;
        assert!(
            !features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature disabled when explicitly disabled"
        );
        assert!(
            !features.response_cache,
            "Telemetry plugin should consider response cache feature disabled when explicitly disabled"
        );

        // Default Values
        let plugin = PluginTestHarness::<Telemetry>::builder()
            .config(include_str!(
                "testdata/full_config_all_features_defaults.router.yaml"
            ))
            .build()
            .with_metrics()
            .await
            .expect("test harness");
        let features = &plugin.enabled_features;
        assert!(
            !features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature disabled when all values are defaulted"
        );
        assert!(
            !features.response_cache,
            "Telemetry plugin should consider response cache feature disabled when all values are defaulted"
        );

        // APQ enabled when default enabled with redis config defined
        let plugin = PluginTestHarness::<Telemetry>::builder()
            .config(include_str!(
                "testdata/full_config_apq_enabled_partial_defaults.router.yaml"
            ))
            .build()
            .with_metrics()
            .await
            .expect("test harness");
        let features = &plugin.enabled_features;
        assert!(
            features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature enabled when top-level enabled flag is defaulted and redis config is defined"
        );

        // APQ disabled when default enabled with redis config NOT defined
        let plugin = PluginTestHarness::<Telemetry>::builder()
            .config(include_str!(
                "testdata/full_config_apq_disabled_partial_defaults.router.yaml"
            ))
            .build()
            .with_metrics()
            .await
            .expect("test harness");
        let features = &plugin.enabled_features;
        assert!(
            !features.distributed_apq_cache,
            "Telemetry plugin should consider apq feature disabled when redis cache is not enabled"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_metrics_ok() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("testdata/custom_attributes.router.yaml"))
                .build()
                .await
                .expect("test harness");
            make_supergraph_request(&plugin).await;

            assert_counter!(
                "http.request",
                1.0,
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_metrics_bad_request() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("testdata/custom_attributes.router.yaml"))
                .build()
                .await
                .expect("test harness");

            let (mock_bad_request_service, mut handle) =
                tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();
            let driver = tokio::spawn(async move {
                let (req, responder) = handle.next_request().await.unwrap();
                responder.send_response(
                    SupergraphResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::BAD_REQUEST)
                        .errors(vec![
                            crate::graphql::Error::builder()
                                .message("nope")
                                .extension_code("NOPE")
                                .build(),
                        ])
                        .build()
                        .unwrap(),
                );
            });
            let mut bad_request_supergraph_service = ServiceBuilder::new()
                .layer(plugin.instrument_supergraph_layer())
                .service(mock_bad_request_service);
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
            crate::plugin::test::await_mock_driver(driver).await;

            assert_counter!(
                "http.request",
                1.0,
                "another_test" = "my_default_value",
                "error" = "nope",
                "myname" = "label_value",
                "renamed_value" = "my_value_set"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_wrong_endpoint() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("testdata/prometheus.router.yaml"))
                .build()
                .await
                .expect("test harness");

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
        let _guard = TEST.lock().await;
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("testdata/prometheus.router.yaml"))
                .build()
                .await
                .expect("test harness");
            plugin.activate();
            u64_histogram!("apollo.test.histo", "it's a test", 1u64);

            make_supergraph_request(&plugin).await;
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_custom_buckets() {
        let _guard = TEST.lock().await;
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "testdata/prometheus_custom_buckets.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");
            plugin.activate();
            u64_histogram!("apollo.test.histo", "it's a test", 1u64);

            make_supergraph_request(&plugin).await;
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_custom_buckets_for_specific_metrics() {
        let _guard = TEST.lock().await;
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "testdata/prometheus_custom_buckets_specific_metrics.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");
            plugin.activate();
            make_supergraph_request(&plugin).await;
            u64_histogram!("apollo.test.histo", "it's a test", 1u64);
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_custom_view_drop() {
        let _guard = TEST.lock().await;
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "testdata/prometheus_custom_view_drop.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");
            make_supergraph_request(&plugin).await;
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
    }

    /// End-to-end: a per-view `cardinality_limit: 2` is wired through the
    /// Prometheus exporter. Recording three distinct attribute sets on the
    /// instrument should overflow on the third, producing an
    /// `otel_metric_overflow="true"` series in the scraped output.
    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_with_cardinality_limit_config() {
        let _guard = TEST.lock().await;
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "testdata/prometheus_cardinality_limit.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");
            plugin.activate();
            u64_histogram!("apollo.test.histo", "it's a test", 1u64, "k" = "a");
            u64_histogram!("apollo.test.histo", "it's a test", 1u64, "k" = "b");
            u64_histogram!("apollo.test.histo", "it's a test", 1u64, "k" = "c");

            make_supergraph_request(&plugin).await;
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics_units_are_included() {
        let _guard = TEST.lock().await;
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("testdata/prometheus.router.yaml"))
                .build()
                .await
                .expect("test harness");
            plugin.activate();
            u64_histogram_with_unit!("apollo.test.histo1", "no unit", "{request}", 1u64);
            f64_histogram_with_unit!("apollo.test.histo2", "unit", "s", 1f64);
            make_supergraph_request(&plugin).await;
            assert_prometheus_metrics!(plugin);
        }
        .with_metrics()
        .await;
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
    fn test_custom_trace_id_propagator_invalid_hex_characters() {
        use crate::test_harness::tracing_test;
        let _guard = tracing_test::dispatcher_guard();

        let header = String::from("x-trace-id");
        let invalid_trace_id = String::from("invalidhexchars");

        let propagator = CustomTraceIdPropagator::new(header.clone(), TraceIdFormat::Uuid);
        let mut headers: HashMap<String, String> = HashMap::new();
        headers.insert(header, invalid_trace_id.clone());

        let span = propagator.extract_span_context(&headers);

        assert!(span.is_none());

        assert!(tracing_test::logs_contain(
            "cannot generate custom trace_id"
        ));

        assert!(tracing_test::logs_contain(&invalid_trace_id));
    }

    #[test]
    fn test_extract_with_context_preserves_existing_context_when_header_absent() {
        let header = String::from("x-trace-id");
        let propagator = CustomTraceIdPropagator::new(header, TraceIdFormat::Uuid);

        let existing_span_context = SpanContext::new(
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            TraceFlags::default().with_sampled(true),
            true,
            TraceState::default(),
        );
        let cx =
            opentelemetry::Context::new().with_remote_span_context(existing_span_context.clone());

        let headers: HashMap<String, String> = HashMap::new();

        let result_cx = propagator.extract_with_context(&cx, &headers);

        assert_eq!(result_cx.span().span_context(), &existing_span_context);
    }

    #[test]
    fn test_extract_with_context_stays_empty_when_header_absent_and_no_prior_context() {
        let header = String::from("x-trace-id");
        let propagator = CustomTraceIdPropagator::new(header, TraceIdFormat::Uuid);

        let cx = opentelemetry::Context::new(); // no propagator has extracted anything yet
        let headers: HashMap<String, String> = HashMap::new(); // custom header absent

        let result_cx = propagator.extract_with_context(&cx, &headers);

        assert_eq!(
            result_cx.span().span_context(),
            &SpanContext::empty_context()
        );
    }

    #[test]
    fn test_extract_with_context_overrides_when_header_present() {
        let header = String::from("x-trace-id");
        let propagator = CustomTraceIdPropagator::new(header.clone(), TraceIdFormat::Uuid);

        // Prior context, as if W3C had already extracted a different traceparent.
        let previous_span_context = SpanContext::new(
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            TraceFlags::default().with_sampled(true),
            true,
            TraceState::default(),
        );
        let cx = opentelemetry::Context::new().with_remote_span_context(previous_span_context);

        // Custom header is present and must take precedence.
        let mut headers: HashMap<String, String> = HashMap::new();
        headers.insert(header, "04f9e396-465c-4840-bc2b-f493b8b1a7fc".to_string());

        let result_cx = propagator.extract_with_context(&cx, &headers);

        assert_eq!(
            result_cx.span().span_context().trace_id().to_string(),
            "04f9e396465c4840bc2bf493b8b1a7fc"
        );
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
                TraceId::from(0x04f9e396465c4840bc2bf493b8b1a7fc),
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

    async fn make_failed_demand_control_request(plugin: &Telemetry, cost_details: CostContext) {
        let (mock_service, mut handle) =
            tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
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

            responder.send_response(
                SupergraphResponse::fake_builder()
                    .context(req.context)
                    .data(
                        serde_json::to_value(graphql::Response::builder().errors(errors).build())
                            .unwrap(),
                    )
                    .build()
                    .unwrap(),
            );
        });

        let mut service = ServiceBuilder::new()
            .layer(plugin.instrument_supergraph_layer())
            .service(mock_service);
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
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_demand_control_delta_filter() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "testdata/demand_control_delta_filter.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");
            make_failed_demand_control_request(
                &plugin,
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_demand_control_result_filter() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "testdata/demand_control_result_filter.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");
            make_failed_demand_control_request(
                &plugin,
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_demand_control_result_attributes() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "testdata/demand_control_result_attribute.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");
            make_failed_demand_control_request(
                &plugin,
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
}

#[cfg(test)]
mod licensed_operation_count_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::Context;
    use crate::apollo_studio_interop::UsageReporting;
    use crate::apollo_studio_interop::UsageReportingOperationDetails;
    use crate::metrics::FutureMetricsExt as _;
    use crate::plugins::telemetry::EnabledFeatures;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::telemetry::apollo_exporter::Sender;
    use crate::query_planner::OperationKind;

    /// Drives `update_apollo_metrics` over a context and returns the licensed operation
    /// count Studio would be billed for.
    async fn licensed_operation_count_for(context: Context) -> u64 {
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        Telemetry::update_apollo_metrics(
            &context,
            0.0,
            Sender::Apollo(tx),
            false,
            Duration::from_millis(1),
            OperationKind::Query,
            None,
            HashMap::new(),
            EnabledFeatures {
                distributed_apq_cache: false,
                response_cache: false,
            },
        );

        rx.recv()
            .await
            .expect("update_apollo_metrics must send a stats report")
            .licensed_operation_count_by_type
            .map(|by_type| by_type.licensed_operation_count)
            .unwrap_or(0)
    }

    /// A context holding no `UsageReporting` bills one licensed operation. Billing does
    /// not depend on the operation reaching execution or reporting anything about
    /// itself.
    #[tokio::test]
    async fn missing_usage_reporting_is_billed_as_one_operation() {
        async {
            assert_eq!(licensed_operation_count_for(Context::new()).await, 1);
        }
        .with_metrics()
        .await;
    }

    /// `UsageReporting::Error` bills nothing: it is the one variant that zeroes the
    /// licensed operation count, so an operation reported with it drops off the bill.
    #[tokio::test]
    async fn usage_reporting_error_is_not_billed() {
        async {
            let context = Context::new();
            context.extensions().with_lock(|lock| {
                lock.insert::<Arc<UsageReporting>>(Arc::new(UsageReporting::Error(
                    "some error key".to_string(),
                )))
            });

            assert_eq!(licensed_operation_count_for(context).await, 0);
        }
        .with_metrics()
        .await;
    }

    /// `UsageReporting::Operation` bills one licensed operation, the same as a context
    /// holding no reporting at all: attribution does not change what an operation costs.
    #[tokio::test]
    async fn operation_details_are_billed_as_one_operation() {
        async {
            let context = Context::new();
            context.extensions().with_lock(|lock| {
                lock.insert::<Arc<UsageReporting>>(Arc::new(UsageReporting::Operation(
                    UsageReportingOperationDetails::default(),
                )))
            });

            assert_eq!(licensed_operation_count_for(context).await, 1);
        }
        .with_metrics()
        .await;
    }
}

#[cfg(test)]
mod reports_to_apollo_tests {
    use serde_json::json;

    use super::reports_to_apollo;

    #[test]
    fn a_telemetry_apollo_section_turns_reporting_on() {
        assert!(reports_to_apollo(
            &json!({ "telemetry": { "apollo": {} } }),
            false
        ));
    }

    #[test]
    fn an_apollo_key_and_graph_ref_turn_reporting_on() {
        assert!(reports_to_apollo(&json!({}), true));
    }

    #[test]
    fn reporting_is_off_without_a_section_or_credentials() {
        assert!(!reports_to_apollo(
            &json!({ "telemetry": { "exporters": {} } }),
            false
        ));
    }
}
