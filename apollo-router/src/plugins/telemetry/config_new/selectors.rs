use std::borrow::Cow;
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::time::Duration;

use schemars::JsonSchema;
use schemars::Schema;
use schemars::SchemaGenerator;
use serde::Deserialize;
use serde::Deserializer;
use serde_json_bytes::path::JsonPathInst;

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) enum OperationName {
    /// The raw operation name.
    String,
    /// A hash of the operation name.
    Hash,
}

#[apollo_configuration::configuration]
#[allow(dead_code)]
#[derive(PartialEq)]
pub(crate) enum ErrorRepr {
    // /// The error code if available
    // Code,
    /// The error reason
    Reason,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
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

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) enum ResponseStatus {
    /// The http status code.
    Code,
    /// The http status reason.
    Reason,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) enum ActiveSubgraphRequests {
    /// The number of active subgraph requests as a count.
    Count,
    /// Whether there are any active subgraph requests as a boolean.
    Bool,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) enum OperationKind {
    /// The raw operation kind.
    String,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
#[serde(untagged)]
pub(crate) enum EntityType {
    #[config(default)]
    All(All),
    Named(String),
}

#[apollo_configuration::configuration]
#[derive(Copy, PartialEq, Eq)]
pub(crate) enum All {
    #[config(default)]
    All,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) enum CacheKind {
    Hit,
    Miss,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) enum CacheStatus {
    Hit,
    Miss,
    PartialHit,
    Status,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) enum CacheControlSelector {
    /// Returns the scope, either `public` or `private`
    Scope,
    /// Boolean to know the value of no-store
    NoStore,
    /// Value of s-maxage or max-age in cache-control
    MaxAge,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
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

/// A JSONPath expression from a selector's configuration, such as `$[0].extensions.code`.
///
/// Parsed when the configuration loads, so an invalid expression is a configuration error.
/// Dereferences to [`JsonPathInst`] for evaluation. Two paths are equal when their source text
/// is equal; the source text is also its `Debug` output.
#[derive(Clone)]
pub(crate) struct SelectorJsonPath {
    source: String,
    path: JsonPathInst,
}

impl FromStr for SelectorJsonPath {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            source: source.to_string(),
            path: JsonPathInst::from_str(source)?,
        })
    }
}

impl Deref for SelectorJsonPath {
    type Target = JsonPathInst;

    fn deref(&self) -> &JsonPathInst {
        &self.path
    }
}

impl fmt::Debug for SelectorJsonPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.source, f)
    }
}

impl PartialEq for SelectorJsonPath {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl<'de> Deserialize<'de> for SelectorJsonPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SelectorJsonPath {
    fn schema_name() -> Cow<'static, str> {
        String::schema_name()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }

    fn inline_schema() -> bool {
        true
    }
}

// Parsing already rejected an invalid expression; there is no further rule to check.
impl apollo_configuration::Validate for SelectorJsonPath {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use opentelemetry::Value;

    use super::DurationUnit;
    use super::SelectorJsonPath;

    #[test]
    fn selector_json_path_compares_and_prints_its_source() {
        let path: SelectorJsonPath = serde_json::from_str(r#""$[0].extensions.code""#).unwrap();
        assert_eq!(format!("{path:?}"), r#""$[0].extensions.code""#);
        assert_eq!(path, "$[0].extensions.code".parse().unwrap());
        assert_ne!(path, "$[1].extensions.code".parse().unwrap());
        assert!(serde_json::from_str::<SelectorJsonPath>(r#""$[""#).is_err());
    }

    #[rstest::rstest]
    #[case(DurationUnit::Seconds, Value::F64(1.0))]
    #[case(DurationUnit::Milliseconds, Value::I64(1000))]
    #[case(DurationUnit::Nanoseconds, Value::I64(1_000_000_000))]
    fn test_duration_unit(#[case] unit: DurationUnit, #[case] expected_value: Value) {
        let duration = Duration::from_secs(1);
        assert_eq!(unit.to_otel_value(duration), expected_value);
    }
}
