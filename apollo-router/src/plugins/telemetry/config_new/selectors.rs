use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use jsonpath_rust::parser::model::Function;
use jsonpath_rust::parser::model::JsonPath;
use jsonpath_rust::parser::model::JsonPathIndex;
use schemars::JsonSchema;
use schemars::Schema;
use schemars::SchemaGenerator;
use serde::Deserialize;
use serde::Deserializer;
use serde_json_bytes::Value;
use serde_json_bytes::path::JsonPathInst;

use super::ToOtelArray;
use super::ToOtelValue;

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
/// Two paths are equal when their source text is equal; the source text is also its `Debug`
/// output.
///
/// A path is *singular* when it is built only from the root, field names and single indexes,
/// optionally ending in `length()` (`$.a`, `$[0].extensions.code`, `$.items.length()`), so it
/// can match at most one value. `$[*].length()` is not singular: it gives one length per match.
/// [`SelectorJsonPath::evaluate`] uses this to decide whether an attribute is a value or an array.
#[derive(Clone)]
pub(crate) struct SelectorJsonPath {
    source: String,
    path: JsonPathInst,
    singular: bool,
}

impl SelectorJsonPath {
    /// Whether the path can match at most one value: see [`SelectorJsonPath`].
    pub(crate) fn is_singular(&self) -> bool {
        self.singular
    }

    /// Every value the path matches in `value`, in document order.
    pub(crate) fn select<'a>(&'a self, value: &'a Value) -> impl Iterator<Item = Cow<'a, Value>> {
        self.path.select(value)
    }

    /// The attribute value the path selects from `value`, or `None` when nothing matches.
    ///
    /// A singular path returns its match converted as a single value. Any other path returns an
    /// array of every non-null match, even when there is only one; matches that do not share one
    /// attribute type become an array of strings, with non-string values written as JSON.
    pub(crate) fn evaluate(&self, value: &Value) -> Option<opentelemetry::Value> {
        if self.singular {
            return self.select(value).next()?.maybe_to_otel_value();
        }
        let matches: Vec<Cow<Value>> = self
            .select(value)
            .filter(|matched| !matched.is_null())
            .collect();
        if matches.is_empty() {
            return None;
        }
        let array =
            Value::maybe_to_otel_array(matches.iter().map(Cow::as_ref)).unwrap_or_else(|| {
                opentelemetry::Array::String(
                    matches
                        .iter()
                        .map(|matched| match_to_string(matched).into())
                        .collect(),
                )
            });
        Some(opentelemetry::Value::Array(array))
    }
}

/// A match as one element of an array of strings: a string as is, any other value as JSON.
pub(crate) fn match_to_string(matched: &Value) -> String {
    match matched.as_str() {
        Some(string) => string.to_string(),
        None => matched.to_string(),
    }
}

/// Whether `path` can match at most one value: see [`SelectorJsonPath`].
fn is_singular(path: &JsonPath) -> bool {
    match path {
        JsonPath::Root | JsonPath::Field(_) | JsonPath::Empty => true,
        JsonPath::Index(JsonPathIndex::Single(_)) => true,
        // Evaluation maps each matched array to its length, one value for one match.
        JsonPath::Fn(Function::Length) => true,
        JsonPath::Chain(segments) => segments.iter().all(is_singular),
        _ => false,
    }
}

impl FromStr for SelectorJsonPath {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            source: source.to_string(),
            path: JsonPathInst::from_str(source)?,
            // The same parser `JsonPathInst` uses, whose syntax tree it keeps private.
            singular: is_singular(&JsonPath::try_from(source)?),
        })
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
    use std::str::FromStr;
    use std::time::Duration;

    use opentelemetry::Array;
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
    #[case::root("$", true)]
    #[case::field("$.a", true)]
    #[case::bracket_field("$['a']", true)]
    #[case::index("$[0].extensions.code", true)]
    #[case::legacy_index("$.[0].extensions.code", true)]
    #[case::length("$.items.length()", true)]
    #[case::wildcard_length("$[*].length()", false)]
    #[case::wildcard("$[*].extensions.code", false)]
    #[case::dot_wildcard("$.*", false)]
    #[case::descendant("$..id", false)]
    #[case::slice("$[0:2]", false)]
    #[case::index_union("$[0,1]", false)]
    #[case::key_union("$['a','b']", false)]
    #[case::filter("$[?(@.id > 1)]", false)]
    fn selector_json_path_is_singular(#[case] path: &str, #[case] singular: bool) {
        assert_eq!(SelectorJsonPath::from_str(path).unwrap().singular, singular);
    }

    #[rstest::rstest]
    #[case::singular_one_match("$.a", Some(Value::from("x")))]
    #[case::singular_no_match("$.missing", None)]
    #[case::singular_matches_array(
        "$.codes",
        Some(Value::Array(Array::String(vec!["A".into(), "B".into()])))
    )]
    #[case::singular_matches_null("$.nothing", None)]
    #[case::singular_matches_array_with_null("$.with_null", Some(Value::from("[1,null]")))]
    #[case::wildcard_one_match(
        "$.one[*].id",
        Some(Value::Array(Array::String(vec!["only".into()])))
    )]
    #[case::wildcard_several_matches("$.items[*].id", Some(Value::Array(Array::I64(vec![1, 2, 3]))))]
    #[case::wildcard_no_match("$.none[*].id", None)]
    #[case::descendant("$..id", Some(Value::Array(Array::String(vec!["only".into(), "1".into(), "2".into(), "3".into()]))))]
    #[case::filter_one_match(
        "$.items[?(@.id > 2)].id",
        Some(Value::Array(Array::I64(vec![3])))
    )]
    #[case::mixed_types(
        "$.mixed[*]",
        Some(Value::Array(Array::String(vec!["1".into(), "a".into(), "[2]".into()])))
    )]
    #[case::nulls_skipped("$.nulls[*]", Some(Value::Array(Array::String(vec!["a".into()]))))]
    #[case::only_nulls("$.only_nulls[*]", None)]
    #[case::wildcard_matches_array(
        "$.nested[*]",
        Some(Value::Array(Array::String(vec![r#"["A","B"]"#.into()])))
    )]
    #[case::length("$.items.length()", Some(Value::I64(3)))]
    #[case::wildcard_length("$.lists[*].length()", Some(Value::Array(Array::I64(vec![2, 1]))))]
    fn selector_json_path_evaluate(#[case] path: &str, #[case] expected: Option<Value>) {
        let data = serde_json_bytes::json!({
            "a": "x",
            "nothing": null,
            "with_null": [1, null],
            "codes": ["A", "B"],
            "one": [{"id": "only"}],
            "items": [{"id": 1}, {"id": 2}, {"id": 3}],
            "none": [],
            "mixed": [1, "a", [2]],
            "nulls": [null, "a"],
            "only_nulls": [null, null],
            "nested": [["A", "B"]],
            "lists": [[1, 2], [3]],
        });
        assert_eq!(
            SelectorJsonPath::from_str(path).unwrap().evaluate(&data),
            expected
        );
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
