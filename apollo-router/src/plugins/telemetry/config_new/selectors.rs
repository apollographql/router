use std::time::Duration;

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

#[derive(Deserialize, JsonSchema, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheControlSelector {
    /// Returns the scope, either `public` or `private`
    Scope,
    /// Boolean to know the value of no-store
    NoStore,
    /// Value of s-maxage or max-age in cache-control
    MaxAge,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum DurationUnit {
    /// Duration in milliseconds (integer)
    Milliseconds,
    /// Duration in seconds (floating point)
    Seconds,
    /// Duration in nanoseconds (integer)
    Nanoseconds,
}

impl DurationUnit {
    pub(crate) fn to_otel_value(&self, duration: Duration) -> opentelemetry::Value {
        match self {
            Self::Milliseconds => {
                opentelemetry::Value::I64(duration.as_millis().try_into().unwrap_or(i64::MAX))
            }
            Self::Seconds => opentelemetry::Value::F64(duration.as_secs_f64()),
            Self::Nanoseconds => {
                opentelemetry::Value::I64(duration.as_nanos().try_into().unwrap_or(i64::MAX))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use opentelemetry::Value;

    use super::DurationUnit;

    #[rstest::rstest]
    #[case(DurationUnit::Seconds, Value::F64(1.0))]
    #[case(DurationUnit::Milliseconds, Value::I64(1000))]
    #[case(DurationUnit::Nanoseconds, Value::I64(1_000_000_000))]
    fn test_duration_unit(#[case] unit: DurationUnit, #[case] expected_value: Value) {
        let duration = Duration::from_secs(1);
        assert_eq!(unit.to_otel_value(duration), expected_value);
    }
}
