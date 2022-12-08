//! Configuration for apollo telemetry.
// This entire file is license key functionality
use std::collections::HashMap;
use std::ops::AddAssign;
use std::time::SystemTime;

use derivative::Derivative;
use http::header::HeaderName;
use itertools::Itertools;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use super::config::ExposeTraceId;
use super::metrics::apollo::studio::ContextualizedStats;
use super::metrics::apollo::studio::SingleStats;
use super::metrics::apollo::studio::SingleStatsReport;
use super::tracing::apollo::TracesReport;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_vec_header_name;
use crate::plugins::telemetry::apollo_exporter::proto::ReferencedFieldsForType;
use crate::plugins::telemetry::apollo_exporter::proto::ReportHeader;
use crate::plugins::telemetry::apollo_exporter::proto::StatsContext;
use crate::plugins::telemetry::apollo_exporter::proto::Trace;
use crate::plugins::telemetry::config::SamplerOption;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;

pub(crate) const ENDPOINT_DEFAULT: &str =
    "https://usage-reporting.api.apollographql.com/api/ingress/traces";

#[derive(Derivative)]
#[derivative(Debug)]
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The Apollo Studio endpoint for exporting traces and metrics.
    #[schemars(with = "String", default = "endpoint_default")]
    #[serde(default = "endpoint_default")]
    pub(crate) endpoint: Url,

    /// The Apollo Studio API key.
    #[schemars(skip)]
    #[serde(skip, default = "apollo_key")]
    pub(crate) apollo_key: Option<String>,

    /// The Apollo Studio graph reference.
    #[schemars(skip)]
    #[serde(skip, default = "apollo_graph_reference")]
    pub(crate) apollo_graph_ref: Option<String>,

    /// The name of the header to extract from requests when populating 'client nane' for traces and metrics in Apollo Studio.
    #[schemars(with = "Option<String>", default = "client_name_header_default_str")]
    #[serde(
        deserialize_with = "deserialize_header_name",
        default = "client_name_header_default"
    )]
    pub(crate) client_name_header: HeaderName,

    /// The name of the header to extract from requests when populating 'client version' for traces and metrics in Apollo Studio.
    #[schemars(with = "Option<String>", default = "client_version_header_default_str")]
    #[serde(
        deserialize_with = "deserialize_header_name",
        default = "client_version_header_default"
    )]
    pub(crate) client_version_header: HeaderName,

    /// The buffer size for sending traces to Apollo. Increase this if you are experiencing lost traces.
    #[serde(default = "default_buffer_size")]
    pub(crate) buffer_size: usize,

    /// Enable field level instrumentation for subgraphs via ftv1. ftv1 tracing can cause performance issues as it is transmitted in band with subgraph responses.
    /// 0.0 will result in no field level instrumentation. 1.0 will result in always instrumentation.
    /// Value MUST be less than global sampling rate
    pub(crate) field_level_instrumentation_sampler: Option<SamplerOption>,

    /// To configure which request header names and values are included in trace data that's sent to Apollo Studio.
    #[serde(default)]
    pub(crate) send_headers: ForwardHeaders,
    /// To configure which GraphQL variable values are included in trace data that's sent to Apollo Studio
    #[serde(default)]
    pub(crate) send_variable_values: ForwardValues,

    // This'll get overridden if a user tries to set it.
    // The purpose is to allow is to pass this in to the plugin.
    #[schemars(skip)]
    pub(crate) schema_id: String,

    // Skipped because only useful at runtime, it's a copy of the configuration in tracing config
    #[schemars(skip)]
    #[serde(skip)]
    pub(crate) expose_trace_id: ExposeTraceId,

    pub(crate) batch_processor: Option<BatchProcessorConfig>,
}

#[cfg(test)]
fn apollo_key() -> Option<String> {
    // During tests we don't want env variables to affect defaults
    None
}

#[cfg(not(test))]
fn apollo_key() -> Option<String> {
    std::env::var("APOLLO_KEY").ok()
}

#[cfg(test)]
fn apollo_graph_reference() -> Option<String> {
    // During tests we don't want env variables to affect defaults
    None
}

#[cfg(not(test))]
fn apollo_graph_reference() -> Option<String> {
    std::env::var("APOLLO_GRAPH_REF").ok()
}

fn endpoint_default() -> Url {
    Url::parse(ENDPOINT_DEFAULT).expect("must be valid url")
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

pub(crate) const fn default_buffer_size() -> usize {
    10000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: Url::parse(ENDPOINT_DEFAULT).expect("default endpoint URL must be parseable"),
            apollo_key: None,
            apollo_graph_ref: None,
            client_name_header: client_name_header_default(),
            client_version_header: client_version_header_default(),
            schema_id: "<no_schema_id>".to_string(),
            buffer_size: default_buffer_size(),
            field_level_instrumentation_sampler: Some(SamplerOption::TraceIdRatioBased(0.01)),
            send_headers: ForwardHeaders::None,
            send_variable_values: ForwardValues::None,
            expose_trace_id: ExposeTraceId::default(),
            batch_processor: Some(BatchProcessorConfig::default()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ForwardHeaders {
    None,
    All,
    #[serde(deserialize_with = "deserialize_vec_header_name")]
    #[schemars(with = "Vec<String>")]
    Only(Vec<HeaderName>),
    #[schemars(with = "Vec<String>")]
    #[serde(deserialize_with = "deserialize_vec_header_name")]
    Except(Vec<HeaderName>),
}

impl Default for ForwardHeaders {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ForwardValues {
    None,
    All,
    Only(Vec<String>),
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
    pub(crate) operation_count: u64,
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

    pub(crate) fn into_report(
        self,
        header: ReportHeader,
    ) -> crate::plugins::telemetry::apollo_exporter::proto::Report {
        let mut report = crate::plugins::telemetry::apollo_exporter::proto::Report {
            header: Some(header),
            end_time: Some(SystemTime::now().into()),
            operation_count: self.operation_count,
            ..Default::default()
        };

        for (key, traces_and_stats) in self.traces_per_query {
            report.traces_per_query.insert(key, traces_and_stats.into());
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
        self.operation_count += report.traces.len() as u64;
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

        self.operation_count += report.operation_count;
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct TracesAndStats {
    pub(crate) traces: Vec<Trace>,
    #[serde(with = "vectorize")]
    pub(crate) stats_with_context: HashMap<StatsContext, ContextualizedStats>,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

impl From<TracesAndStats> for crate::plugins::telemetry::apollo_exporter::proto::TracesAndStats {
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
