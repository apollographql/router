//! Configuration for apollo telemetry.
// This entire file is license key functionality
use std::collections::HashMap;
use std::ops::AddAssign;
use std::time::Duration;
use std::time::SystemTime;

use apollo_spaceport::ReferencedFieldsForType;
use apollo_spaceport::ReportHeader;
use apollo_spaceport::StatsContext;
use apollo_spaceport::Trace;
use derivative::Derivative;
use http::header::HeaderName;
use itertools::Itertools;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ulid::Ulid;
use url::Url;

use super::apollo_exporter::EntryTTL;
use super::apollo_exporter::Sender;
use super::metrics::apollo::studio::ContextualizedStats;
use super::metrics::apollo::studio::SingleStats;
use super::metrics::apollo::studio::SingleStatsReport;
use super::tracing::apollo::TracesReport;
use crate::http_ext::RequestId;
use crate::plugin::serde::deserialize_header_name;

// TTL for orphan traces/metrics
// An orphan is a metric without an associated trace or contrary
const ORPHANS_TTL: Duration = Duration::from_secs(13);

#[derive(Derivative)]
#[derivative(Debug)]
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The Apollo Studio endpoint for exporting traces and metrics.
    #[schemars(with = "Option<String>")]
    pub(crate) endpoint: Option<Url>,

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
    #[serde(default)]
    pub(crate) field_level_instrumentation: bool,

    /// To configure which request header names and values are included in trace data that's sent to Apollo Studio.
    #[serde(default)]
    pub(crate) send_headers: ForwardValues,
    /// To configure which GraphQL variable values are included in trace data that's sent to Apollo Studio
    #[serde(default)]
    pub(crate) send_variable_values: ForwardValues,

    // This'll get overridden if a user tries to set it.
    // The purpose is to allow is to pass this in to the plugin.
    #[schemars(skip)]
    pub(crate) schema_id: String,
    #[schemars(skip)]
    #[serde(skip)]
    #[derivative(Debug = "ignore")]
    pub(crate) apollo_sender: Sender,
}

fn apollo_key() -> Option<String> {
    std::env::var("APOLLO_KEY").ok()
}

fn apollo_graph_reference() -> Option<String> {
    std::env::var("APOLLO_GRAPH_REF").ok()
}

fn client_name_header_default_str() -> &'static str {
    "apollographql-client-name"
}

fn client_name_header_default() -> HeaderName {
    HeaderName::from_static(client_name_header_default_str())
}

fn client_version_header_default_str() -> &'static str {
    "apollographql-client-version"
}

fn client_version_header_default() -> HeaderName {
    HeaderName::from_static(client_version_header_default_str())
}

fn default_buffer_size() -> usize {
    10000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: None,
            apollo_key: None,
            apollo_graph_ref: None,
            client_name_header: client_name_header_default(),
            client_version_header: client_version_header_default(),
            schema_id: "<no_schema_id>".to_string(),
            apollo_sender: Sender::default(),
            buffer_size: 10000,
            field_level_instrumentation: false,
            send_headers: ForwardValues::None,
            send_variable_values: ForwardValues::None,
        }
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
    Stats(EntryTTL<SingleStatsReport>),
    Traces(EntryTTL<TracesReport>),
}

#[derive(Default)]
pub(crate) struct ReportBuilder {
    // signature / trace by request_id
    pub(crate) traces: HashMap<String, EntryTTL<(String, Trace)>>,
    // Buffer all (signatures and stats) by request_id to not have both traces and stats
    pub(crate) stats: HashMap<String, EntryTTL<HashMap<String, SingleStats>>>,
    pub(crate) report: Report,
}

impl ReportBuilder {
    pub(crate) fn build(mut self) -> (Report, Vec<SingleReport>) {
        // implement merge strategy
        let duplicated_keys_for_reqs: Vec<String> = self
            .traces
            .keys()
            .chain(self.stats.keys())
            .duplicates()
            .cloned()
            .collect();

        for duplicated_key in duplicated_keys_for_reqs {
            let (operation_signature, trace) = self
                .traces
                .remove(&duplicated_key)
                .expect("must exist because it's a duplicate key")
                .inner;
            let _stats = self
                .stats
                .remove(&duplicated_key)
                .expect("must exist because it's a duplicate key")
                .inner;

            let entry = self
                .report
                .traces_per_query
                .entry(operation_signature)
                .or_default();

            // Because if we have traces we can't also provide metrics because it's computed as 2 requests in Studio
            self.report.operation_count += 1;
            entry.traces.push(trace);
        }

        // This part is to handle orphans, which means metrics that has been added without traces
        // If we have metrics without corresponding traces we put them in an orphans vec
        // It stored in an EntryTTL to set a TTL on elements, once it has reached the TTL
        // it means we can send the metrics because we know there isn't any associated trace
        let mut orphans = Vec::new();
        for (request_id, entry) in self.stats {
            // These stats reached TTL without finding corresponding traces then send it
            if entry.created.elapsed() > ORPHANS_TTL {
                for (key, stats) in entry.inner {
                    *self.report.traces_per_query.entry(key).or_default() += stats;
                }
                self.report.operation_count += 1;
            } else {
                orphans.push(SingleReport::Stats(entry.map(|e| SingleStatsReport {
                    request_id: RequestId(
                        Ulid::from_string(&request_id).expect("has already been parsed before"),
                    ),
                    stats: e,
                    operation_count: 1,
                })));
            }
        }

        for (request_id, entry) in self.traces {
            // This trace reached TTL without finding corresponding metrics then send it
            if entry.created.elapsed() > ORPHANS_TTL {
                self.report += TracesReport {
                    traces: HashMap::from([(request_id, entry.inner)]),
                };
            } else {
                orphans.push(SingleReport::Traces(entry.map(|e| TracesReport {
                    traces: HashMap::from([(request_id.clone(), e)]),
                })));
            }
        }

        (self.report, orphans)
    }
}

impl AddAssign<SingleReport> for ReportBuilder {
    fn add_assign(&mut self, report: SingleReport) {
        match report {
            SingleReport::Stats(stats) => self.add_assign(stats),
            SingleReport::Traces(traces) => self.add_assign(traces),
        }
    }
}

impl AddAssign<EntryTTL<SingleStatsReport>> for ReportBuilder {
    fn add_assign(&mut self, report: EntryTTL<SingleStatsReport>) {
        self.stats
            .insert(report.request_id.to_string(), report.map(|r| r.stats));
    }
}

impl AddAssign<EntryTTL<TracesReport>> for ReportBuilder {
    fn add_assign(&mut self, report: EntryTTL<TracesReport>) {
        self.traces.extend(
            report
                .inner
                .traces
                .into_iter()
                .map(|(k, v)| (k, EntryTTL::new(v, report.created))),
        );
    }
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

    pub(crate) fn into_report(self, header: ReportHeader) -> apollo_spaceport::Report {
        let mut report = apollo_spaceport::Report {
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

impl AddAssign<TracesReport> for Report {
    fn add_assign(&mut self, report: TracesReport) {
        self.operation_count += report.traces.len() as u64;
        for (_request_id, (operation_signature, trace)) in report.traces {
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

impl From<TracesAndStats> for apollo_spaceport::TracesAndStats {
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
