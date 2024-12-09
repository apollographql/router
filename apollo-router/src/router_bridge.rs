use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "camelCase")]
/// The error location
pub struct Location {
    /// The line number
    pub line: u32,
    /// The column number
    pub column: u32,
}

/// Options for planning a query
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlanOptions {
    /// Which labels to override during query planning
    pub(crate) override_conditions: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "camelCase")]
/// A list of fields that will be resolved
/// for a given type
pub(crate) struct ReferencedFieldsForType {
    /// names of the fields queried
    #[serde(default)]
    pub(crate) field_names: Vec<String>,
    /// whether the field is an interface
    #[serde(default)]
    pub(crate) is_interface: bool,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "camelCase")]
/// UsageReporting fields, that will be used
/// to send stats to uplink/studio
pub(crate) struct UsageReporting {
    /// The `stats_report_key` is a unique identifier derived from schema and query.
    /// Metric data  sent to Studio must be aggregated
    /// via grouped key of (`client_name`, `client_version`, `stats_report_key`).
    pub(crate) stats_report_key: String,
    /// a list of all types and fields referenced in the query
    #[serde(default)]
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}
