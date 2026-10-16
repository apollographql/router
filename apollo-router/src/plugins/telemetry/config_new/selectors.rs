use std::time::Duration;

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
