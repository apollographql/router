//! Configuration for apollo telemetry.
use std::collections::HashMap;
use std::fmt::Display;
use std::num::NonZeroUsize;
use std::ops::AddAssign;
use std::time::SystemTime;

use derivative::Derivative;
use http::header::HeaderName;
use itertools::Itertools;
use schemars::JsonSchema;
use serde::ser::SerializeMap;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use super::metrics::apollo::studio::ContextualizedStats;
use super::metrics::apollo::studio::SingleStats;
use super::metrics::apollo::studio::SingleStatsReport;
use super::tracing::apollo::TracesReport;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_vec_header_name;
use crate::plugins::telemetry::apollo_exporter::proto::reports::ReferencedFieldsForType;
use crate::plugins::telemetry::apollo_exporter::proto::reports::ReportHeader;
use crate::plugins::telemetry::apollo_exporter::proto::reports::StatsContext;
use crate::plugins::telemetry::apollo_exporter::proto::reports::Trace;
use crate::plugins::telemetry::config::SamplerOption;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::query_planner::OperationKind;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;

pub(crate) const ENDPOINT_DEFAULT: &str =
    "https://usage-reporting.api.apollographql.com/api/ingress/traces";

pub(crate) const OTLP_ENDPOINT_DEFAULT: &str = "http://0.0.0.0:4317";

#[derive(Derivative)]
#[derivative(Debug)]
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Config {
    /// The Apollo Studio endpoint for exporting traces and metrics.
    #[schemars(with = "String", default = "endpoint_default")]
    pub(crate) endpoint: Url,

    /// The Apollo Studio endpoint for exporting traces and metrics.
    #[schemars(with = "String", default = "otlp_endpoint_default")]
    pub(crate) experimental_otlp_endpoint: Url,

    /// The Apollo Studio API key.
    #[schemars(skip)]
    #[serde(skip)]
    pub(crate) apollo_key: Option<String>,

    /// The Apollo Studio graph reference.
    #[schemars(skip)]
    #[serde(skip)]
    pub(crate) apollo_graph_ref: Option<String>,

    /// The name of the header to extract from requests when populating 'client nane' for traces and metrics in Apollo Studio.
    #[schemars(with = "Option<String>", default = "client_name_header_default_str")]
    #[serde(deserialize_with = "deserialize_header_name")]
    pub(crate) client_name_header: HeaderName,

    /// The name of the header to extract from requests when populating 'client version' for traces and metrics in Apollo Studio.
    #[schemars(with = "Option<String>", default = "client_version_header_default_str")]
    #[serde(deserialize_with = "deserialize_header_name")]
    pub(crate) client_version_header: HeaderName,

    /// The buffer size for sending traces to Apollo. Increase this if you are experiencing lost traces.
    pub(crate) buffer_size: NonZeroUsize,

    /// Field level instrumentation for subgraphs via ftv1. ftv1 tracing can cause performance issues as it is transmitted in band with subgraph responses.
    pub(crate) field_level_instrumentation_sampler: SamplerOption,

    /// To configure which request header names and values are included in trace data that's sent to Apollo Studio.
    pub(crate) send_headers: ForwardHeaders,
    /// To configure which GraphQL variable values are included in trace data that's sent to Apollo Studio
    pub(crate) send_variable_values: ForwardValues,

    // This'll get overridden if a user tries to set it.
    // The purpose is to allow is to pass this in to the plugin.
    #[schemars(skip)]
    pub(crate) schema_id: String,

    /// Configuration for batch processing.
    pub(crate) batch_processor: BatchProcessorConfig,

    /// Configure the way errors are transmitted to Apollo Studio
    pub(crate) errors: ErrorsConfiguration,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ErrorsConfiguration {
    /// Handling of errors coming from subgraph
    pub(crate) subgraph: SubgraphErrorConfig,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphErrorConfig {
    /// Handling of errors coming from all subgraphs
    pub(crate) all: ErrorConfiguration,
    /// Handling of errors coming from specified subgraphs
    pub(crate) subgraphs: Option<HashMap<String, ErrorConfiguration>>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ErrorConfiguration {
    /// Send subgraph errors to Apollo Studio
    pub(crate) send: bool,
    /// Redact subgraph errors to Apollo Studio
    pub(crate) redact: bool,
}

impl Default for ErrorConfiguration {
    fn default() -> Self {
        Self {
            send: default_send_errors(),
            redact: default_redact_errors(),
        }
    }
}

impl SubgraphErrorConfig {
    pub(crate) fn get_error_config(&self, subgraph: &str) -> &ErrorConfiguration {
        if let Some(subgraph_conf) = self.subgraphs.as_ref().and_then(|s| s.get(subgraph)) {
            subgraph_conf
        } else {
            &self.all
        }
    }
}

pub(crate) const fn default_send_errors() -> bool {
    true
}

pub(crate) const fn default_redact_errors() -> bool {
    true
}

const fn default_field_level_instrumentation_sampler() -> SamplerOption {
    SamplerOption::TraceIdRatioBased(0.01)
}

fn endpoint_default() -> Url {
    Url::parse(ENDPOINT_DEFAULT).expect("must be valid url")
}

fn otlp_endpoint_default() -> Url {
    Url::parse(OTLP_ENDPOINT_DEFAULT).expect("must be valid url")
}

const fn client_name_header_default_str() -> &'static str {
    "apollographql-client-name"
}

const fn client_name_header_default() -> HeaderName {
    HeaderName::from_static(client_name_header_default_str())
}

const fn client_version_header_default_str() -> &'static str {
    "apollographql-client-version"
}

const fn client_version_header_default() -> HeaderName {
    HeaderName::from_static(client_version_header_default_str())
}

pub(crate) const fn default_buffer_size() -> NonZeroUsize {
    unsafe { NonZeroUsize::new_unchecked(10000) }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: endpoint_default(),
            experimental_otlp_endpoint: otlp_endpoint_default(),
            apollo_key: apollo_key(),
            apollo_graph_ref: apollo_graph_reference(),
            client_name_header: client_name_header_default(),
            client_version_header: client_version_header_default(),
            schema_id: "<no_schema_id>".to_string(),
            buffer_size: default_buffer_size(),
            field_level_instrumentation_sampler: default_field_level_instrumentation_sampler(),
            send_headers: ForwardHeaders::None,
            send_variable_values: ForwardValues::None,
            batch_processor: BatchProcessorConfig::default(),
            errors: ErrorsConfiguration::default(),
        }
    }
}

schemar_fn!(
    forward_headers_only,
    Vec<String>,
    "Send only the headers specified"
);
schemar_fn!(
    forward_headers_except,
    Vec<String>,
    "Send all headers except those specified"
);

/// Forward headers
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ForwardHeaders {
    /// Don't send any headers
    None,

    /// Send all headers
    All,

    /// Send only the headers specified
    #[schemars(schema_with = "forward_headers_only")]
    #[serde(deserialize_with = "deserialize_vec_header_name")]
    Only(Vec<HeaderName>),

    /// Send all headers except those specified
    #[schemars(schema_with = "forward_headers_except")]
    #[serde(deserialize_with = "deserialize_vec_header_name")]
    Except(Vec<HeaderName>),
}

impl Default for ForwardHeaders {
    fn default() -> Self {
        Self::None
    }
}

schemar_fn!(
    forward_variables_except,
    Vec<String>,
    "Send all variables except those specified"
);

schemar_fn!(
    forward_variables_only,
    Vec<String>,
    "Send only the variables specified"
);

/// Forward GraphQL variables
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ForwardValues {
    /// Dont send any variables
    None,
    /// Send all variables
    All,
    /// Send only the variables specified
    #[schemars(schema_with = "forward_variables_only")]
    Only(Vec<String>),
    /// Send all variables except those specified
    #[schemars(schema_with = "forward_variables_except")]
    Except(Vec<String>),
}

impl Default for ForwardValues {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Serialize)]
pub(crate) enum SingleReport {
    Stats(SingleStatsReport),
    Traces(TracesReport),
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct Report {
    pub(crate) traces_per_query: HashMap<String, TracesAndStats>,
    #[serde(serialize_with = "serialize_licensed_operation_count_by_type")]
    pub(crate) licensed_operation_count_by_type:
        HashMap<(OperationKind, Option<OperationSubType>), LicensedOperationCountByType>,
}

#[derive(Clone, Default, Debug, Serialize, PartialEq, Eq, Hash)]
pub(crate) struct LicensedOperationCountByType {
    pub(crate) r#type: OperationKind,
    pub(crate) subtype: Option<OperationSubType>,
    pub(crate) licensed_operation_count: u64,
}

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OperationSubType {
    SubscriptionEvent,
    SubscriptionRequest,
}

impl OperationSubType {
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            OperationSubType::SubscriptionEvent => "subscription-event",
            OperationSubType::SubscriptionRequest => "subscription-request",
        }
    }
}

impl Display for OperationSubType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationSubType::SubscriptionEvent => write!(f, "subscription-event"),
            OperationSubType::SubscriptionRequest => write!(f, "subscription-request"),
        }
    }
}

impl From<LicensedOperationCountByType>
    for crate::plugins::telemetry::apollo_exporter::proto::reports::report::OperationCountByType
{
    fn from(value: LicensedOperationCountByType) -> Self {
        Self {
            r#type: value.r#type.as_apollo_operation_type().to_string(),
            subtype: value.subtype.map(|s| s.to_string()).unwrap_or_default(),
            operation_count: value.licensed_operation_count,
        }
    }
}

fn serialize_licensed_operation_count_by_type<S>(
    elt: &HashMap<(OperationKind, Option<OperationSubType>), LicensedOperationCountByType>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut map_ser = serializer.serialize_map(Some(elt.len()))?;
    for ((op_type, op_subtype), v) in elt {
        map_ser.serialize_entry(
            &format!(
                "{}{}",
                op_type.as_apollo_operation_type(),
                op_subtype
                    .map(|o| "/".to_owned() + o.as_str())
                    .unwrap_or_default()
            ),
            v,
        )?;
    }
    map_ser.end()
}

impl Report {
    #[cfg(test)]
    pub(crate) fn new(reports: Vec<SingleStatsReport>) -> Report {
        let mut aggregated_report = Report::default();
        for report in reports {
            aggregated_report += report;
        }
        aggregated_report
    }

    pub(crate) fn build_proto_report(
        &self,
        header: ReportHeader,
    ) -> crate::plugins::telemetry::apollo_exporter::proto::reports::Report {
        let mut report = crate::plugins::telemetry::apollo_exporter::proto::reports::Report {
            header: Some(header),
            end_time: Some(SystemTime::now().into()),
            operation_count_by_type: self
                .licensed_operation_count_by_type
                .values()
                .cloned()
                .map(|op| op.into())
                .collect(),
            traces_pre_aggregated: true,
            ..Default::default()
        };

        for (key, traces_and_stats) in &self.traces_per_query {
            report
                .traces_per_query
                .insert(key.clone(), traces_and_stats.clone().into());
        }
        report
    }
}

impl AddAssign<SingleReport> for Report {
    fn add_assign(&mut self, report: SingleReport) {
        match report {
            SingleReport::Stats(stats) => self.add_assign(stats),
            SingleReport::Traces(traces) => self.add_assign(traces),
        }
    }
}

impl AddAssign<TracesReport> for Report {
    fn add_assign(&mut self, report: TracesReport) {
        // Note that operation count is dealt with in metrics so we don't increment this.
        for (operation_signature, trace) in report.traces {
            self.traces_per_query
                .entry(operation_signature)
                .or_default()
                .traces
                .push(trace);
        }
    }
}

impl AddAssign<SingleStatsReport> for Report {
    fn add_assign(&mut self, report: SingleStatsReport) {
        for (k, v) in report.stats {
            *self.traces_per_query.entry(k).or_default() += v;
        }

        if let Some(licensed_operation_count_by_type) = report.licensed_operation_count_by_type {
            let key = (
                licensed_operation_count_by_type.r#type,
                licensed_operation_count_by_type.subtype,
            );
            self.licensed_operation_count_by_type
                .entry(key)
                .and_modify(|e| {
                    e.licensed_operation_count += 1;
                })
                .or_insert(licensed_operation_count_by_type);
        }
    }
}

#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct TracesAndStats {
    pub(crate) traces: Vec<Trace>,
    #[serde(with = "vectorize")]
    pub(crate) stats_with_context: HashMap<StatsContext, ContextualizedStats>,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

impl From<TracesAndStats>
    for crate::plugins::telemetry::apollo_exporter::proto::reports::TracesAndStats
{
    fn from(stats: TracesAndStats) -> Self {
        Self {
            stats_with_context: stats.stats_with_context.into_values().map_into().collect(),
            referenced_fields_by_type: stats.referenced_fields_by_type,
            trace: stats.traces,
            ..Default::default()
        }
    }
}

impl AddAssign<SingleStats> for TracesAndStats {
    fn add_assign(&mut self, stats: SingleStats) {
        *self
            .stats_with_context
            .entry(stats.stats_with_context.context.clone())
            .or_default() += stats.stats_with_context;

        // No merging required here because references fields by type will always be the same for each stats report key.
        self.referenced_fields_by_type = stats.referenced_fields_by_type;
    }
}

pub(crate) mod vectorize {
    use serde::Serialize;
    use serde::Serializer;

    pub(crate) fn serialize<'a, T, K, V, S>(target: T, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: IntoIterator<Item = (&'a K, &'a V)>,
        K: Serialize + 'a,
        V: Serialize + 'a,
    {
        let container: Vec<_> = target.into_iter().collect();
        serde::Serialize::serialize(&container, ser)
    }
}
