use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationName {
    /// The raw operation name.
    String,
    /// A hash of the operation name.
    Hash,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ErrorRepr {
    // /// The error code if available
    // Code,
    /// The error reason
    Reason,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Query {
    /// The raw query kind.
    String,
    /// The query aliases.
    Aliases,
    /// The query depth.
    Depth,
    /// The query height.
    Height,
    /// The query root fields.
    RootFields,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ResponseStatus {
    /// The http status code.
    Code,
    /// The http status reason.
    Reason,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ActiveSubgraphRequests {
    /// The number of active subgraph requests as a count.
    Count,
    /// Whether there are any active subgraph requests as a boolean.
    Bool,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationKind {
    /// The raw operation kind.
    String,
}

#[derive(Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case", untagged)]
pub(crate) enum EntityType {
    All(All),
    Named(String),
}

impl Default for EntityType {
    fn default() -> Self {
        Self::All(All::All)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum All {
    #[default]
    All,
}

#[derive(Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheKind {
    Hit,
    Miss,
}

#[derive(Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheStatus {
    Hit,
    Miss,
    PartialHit,
    Status,
}
