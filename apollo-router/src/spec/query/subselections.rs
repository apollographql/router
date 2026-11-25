use std::collections::HashMap;
use std::collections::hash_map::Entry;

use serde::Deserialize;
use serde::Serialize;
use serde::de::Visitor;
use serde_json_bytes::ByteString;

use super::DeferStats;
use super::Operation;
use crate::Configuration;
use crate::spec::Condition;
use crate::spec::FieldType;
use crate::spec::Fragment;
use crate::spec::IncludeSkip;
use crate::spec::SpecError;
use crate::spec::selection::Selection;

#[derive(Debug, PartialEq, Hash, Eq)]
pub(crate) struct SubSelectionKey {
    pub(crate) defer_label: Option<String>,
    pub(crate) defer_conditions: BooleanValues,
    /// Response keys from the operation root to the deferred fragment. A labeled `@defer` in a
    /// fragment spread multiple times yields one subselection per spread position, sharing a
    /// label and distinguished by this path.
    pub(crate) defer_path: Vec<String>,
}

// Do not replace this with a derived Serialize implementation
// SubSelectionKey must serialize to a string because it is used as a JSON object key
impl Serialize for SubSelectionKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        debug_assert!(
            !self.defer_path.iter().any(|segment| segment.contains('|')),
            "defer_path segments must not contain the `|` separator"
        );
        let s = format!(
            "{:?}|{}|{}",
            self.defer_conditions.bits,
            self.defer_label.as_deref().unwrap_or(""),
            self.defer_path.join("/")
        );
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for SubSelectionKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(SubSelectionKeyVisitor)
    }
}

struct SubSelectionKeyVisitor;
impl Visitor<'_> for SubSelectionKeyVisitor {
    type Value = SubSelectionKey;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter
            .write_str("a string containing the defer conditions, label and path separated by |")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if let Some((bits_str, rest)) = s.split_once('|') {
            // Split from the right: `@defer(label:)` is an arbitrary string and may contain `|`,
            // while path segments are response keys, which cannot. (The fallback parses the
            // pre-`defer_path` two-field format, where the whole remainder is the label.)
            let (label, path) = rest.rsplit_once('|').unwrap_or((rest, ""));
            Ok(SubSelectionKey {
                defer_conditions: BooleanValues {
                    bits: bits_str
                        .parse::<u32>()
                        .map_err(|_| E::custom("expected a number"))?,
                },
                defer_label: if label.is_empty() {
                    None
                } else {
                    Some(label.to_string())
                },
                defer_path: if path.is_empty() {
                    Vec::new()
                } else {
                    path.split('/').map(str::to_owned).collect()
                },
            })
        } else {
            Err(E::custom("invalid subselection"))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SubSelectionValue {
    pub(crate) selection_set: Vec<Selection>,
    pub(crate) type_name: String,
}

impl SubSelectionValue {
    /// Current selection at a defer_path.
    pub(crate) fn selection_set_at(&self, defer_path: &[String]) -> Option<&[Selection]> {
        let mut selection_set = self.selection_set.as_slice();
        for response_key in defer_path {
            selection_set = match selection_set {
                [
                    Selection::Field {
                        name,
                        alias,
                        selection_set: Some(nested),
                        ..
                    },
                ] if alias.as_ref().unwrap_or(name).as_str() == response_key => nested,
                _ => return None,
            };
        }
        Some(selection_set)
    }
}

/// The values of boolean variables used for conditional `@defer`, packed least-significant-bit
/// first in iteration order of `DeferStats::conditional_defer_variable_names`.
#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Hash, Eq)]
#[serde(transparent)]
pub(crate) struct BooleanValues {
    pub(crate) bits: u32,
}

impl BooleanValues {
    fn eval(&self, variable_name: &str, defer_stats: &DeferStats) -> bool {
        let index = match defer_stats
            .conditional_defer_variable_names
            .get_index_of(variable_name)
        {
            Some(index) => index,
            None => return false,
        };
        (self.bits & (1 << index)) != 0
    }
}

pub(crate) const DEFER_DIRECTIVE_NAME: &str = "defer";

/// We generate subselections for all 2^N possible combinations of these boolean variables.
/// Refuse to do so for a number of combinatinons we deem unreasonable.
const MAX_DEFER_VARIABLES: usize = 4;

/// Collects the subselections that format the primary and deferred responses.
pub(crate) fn collect_subselections(
    configuration: &Configuration,
    operation: &Operation,
    fragments: &HashMap<String, Fragment>,
    defer_stats: &DeferStats,
) -> Result<HashMap<SubSelectionKey, SubSelectionValue>, SpecError> {
    if !configuration.supergraph.defer_support || !defer_stats.has_defer {
        return Ok(HashMap::new());
    }
    if defer_stats.conditional_defer_variable_names.len() > MAX_DEFER_VARIABLES {
        // TODO: dedicated error variant?
        return Err(SpecError::TransformError(
            "@defer conditional on too many different variables".into(),
        ));
    }
    let mut subselections = HashMap::new();
    for defer_conditions in variable_combinations(defer_stats) {
        let context = DeferContext {
            fragments,
            defer_stats,
            defer_conditions,
        };
        context.collect_operation(operation, &mut subselections)?;
    }
    Ok(subselections)
}

/// Returns an iterator of functions, one per combination of boolean values of the given variables.
/// The function return whether a given variable (by its name) is true in that combination.
fn variable_combinations(defer_stats: &DeferStats) -> impl Iterator<Item = BooleanValues> + use<> {
    // `N = variables.len()` boolean values have a total of 2^N combinations.
    // If we enumerate them by counting from 0 to 2^N - 1,
    // interpreting the N bits of the binary representation of the counter
    // as separate boolean values yields all combinations.
    // Indices within the `IndexSet` are integers from 0 to N-1,
    // and so can be used as bit offset within the counter.

    let combinations_count = 1 << defer_stats.conditional_defer_variable_names.len();
    let initial = if defer_stats.has_unconditional_defer {
        // Include the `bits == 0` case where all boolean variables are false.
        // We’ll still generate subselections for remaining (unconditional) @defer
        0
    } else {
        // Exclude that case, because it doesn’t have @defer at all
        1
    };
    let combinations = initial..combinations_count;
    combinations.map(|bits| BooleanValues { bits })
}

/// Normalize a deferred response's path to the field response keys of `defer_path`: array
/// indices and type conditions appear in response paths but not in query-derived paths.
pub(crate) fn defer_path_from_response_path(path: &crate::json_ext::Path) -> Vec<String> {
    path.0
        .iter()
        .filter_map(|element| match element {
            crate::json_ext::PathElement::Key(key, _) => Some(key.clone()),
            _ => None,
        })
        .collect()
}

/// Defer context for a given operation.
struct DeferContext<'a> {
    fragments: &'a HashMap<String, Fragment>,
    defer_stats: &'a DeferStats,
    defer_conditions: BooleanValues,
}

/// An operation split into `primary` and `deferred` selection sets.
#[derive(Default)]
struct Collected {
    primary: Vec<Selection>,
    deferred: Vec<Deferred>,
}

/// A selection that will be deferred as part of this response.
struct Deferred {
    label: Option<String>,
    reversed_path: Vec<String>,
    selection_set: Vec<Selection>,
    type_name: String,
}

impl DeferContext<'_> {
    fn collect_operation(
        &self,
        operation: &Operation,
        subselections: &mut HashMap<SubSelectionKey, SubSelectionValue>,
    ) -> Result<(), SpecError> {
        let collected =
            self.collect_selection_set(&operation.type_name, &operation.selection_set)?;
        for deferred in collected.deferred {
            let (key, value) = deferred.into_subselection(self.defer_conditions);
            match subselections.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(value);
                }
                // `apollo-compiler` validates the uniqueness of defer labels,
                // and the router's labeler gives every unlabeled `@defer` its
                // own label, so a shared key here means a shared `@defer` with the
                // same selections.
                Entry::Occupied(entry) => {
                    debug_assert_eq!(entry.get().selection_set, value.selection_set);
                    debug_assert_eq!(entry.get().type_name, value.type_name);
                }
            }
        }
        if !collected.primary.is_empty() {
            subselections.insert(
                SubSelectionKey {
                    defer_label: None,
                    defer_conditions: self.defer_conditions,
                    defer_path: Vec::new(),
                },
                SubSelectionValue {
                    selection_set: collected.primary,
                    type_name: operation.type_name.clone(),
                },
            );
        }
        Ok(())
    }

    fn is_deferred(&self, defer: &Condition) -> bool {
        match defer {
            Condition::Yes => true,
            Condition::No => false,
            Condition::Variable(name) => self.defer_conditions.eval(name, self.defer_stats),
        }
    }

    fn collect_selection_set(
        &self,
        parent_type: &str,
        selection_set: &[Selection],
    ) -> Result<Collected, SpecError> {
        let mut collected = Collected::default();
        for selection in selection_set {
            match selection {
                Selection::Field {
                    selection_set: None,
                    ..
                } => collected.primary.push(selection.clone()),
                Selection::Field {
                    name,
                    alias,
                    selection_set: Some(selection_set),
                    field_type,
                    include_skip,
                } => {
                    let nested = self.collect_selection_set(
                        field_type.0.inner_named_type().as_str(),
                        selection_set,
                    )?;
                    collected.deferred.extend(
                        nested
                            .deferred
                            .into_iter()
                            .map(|deferred| deferred.wrap_in_field(name, alias, field_type)),
                    );
                    // Leave the field out if everything in it is deferred
                    if !nested.primary.is_empty() {
                        collected.primary.push(Selection::Field {
                            name: name.clone(),
                            alias: alias.clone(),
                            selection_set: Some(nested.primary),
                            field_type: field_type.clone(),
                            include_skip: include_skip.clone(),
                        });
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    include_skip,
                    defer,
                    defer_label,
                    known_type,
                    selection_set,
                } => {
                    let fragment_type = known_type.as_deref().unwrap_or(parent_type);
                    let nested =
                        self.collect_fragment(fragment_type, defer, defer_label, selection_set)?;
                    collected.deferred.extend(nested.deferred);
                    if !nested.primary.is_empty() {
                        collected.primary.push(Selection::InlineFragment {
                            type_condition: type_condition.clone(),
                            include_skip: include_skip.clone(),
                            defer: defer.clone(),
                            defer_label: defer_label.clone(),
                            known_type: known_type.clone(),
                            selection_set: nested.primary,
                        });
                    }
                }
                Selection::FragmentSpread {
                    name,
                    known_type,
                    include_skip,
                    defer,
                    defer_label,
                } => {
                    let fragment = self.fragments.get(name).ok_or_else(|| {
                        SpecError::TransformError("Missing fragment definition".into())
                    })?;
                    let nested = self.collect_fragment(
                        &fragment.type_condition,
                        defer,
                        defer_label,
                        &fragment.selection_set,
                    )?;
                    collected.deferred.extend(nested.deferred);
                    if !nested.primary.is_empty() {
                        // Convert the fragment spread to an inline fragment
                        // so that subselections are self-contained
                        collected.primary.push(Selection::InlineFragment {
                            type_condition: fragment.type_condition.clone(),
                            include_skip: include_skip.clone(),
                            defer: defer.clone(),
                            defer_label: defer_label.clone(),
                            known_type: known_type.clone(),
                            selection_set: nested.primary,
                        });
                    }
                }
            }
        }
        Ok(collected)
    }

    /// Collects the selections of an inline fragment, or of a fragment spread's definition.
    ///
    /// The returned `primary` holds the fragment's selections that aren't
    /// deferred to be later inserted in an inline fragment.
    fn collect_fragment(
        &self,
        fragment_type: &str,
        defer: &Condition,
        defer_label: &Option<String>,
        selection_set: &[Selection],
    ) -> Result<Collected, SpecError> {
        let mut collected = self.collect_selection_set(fragment_type, selection_set)?;
        // `primary` is empty if this fragment is deferred.
        if self.is_deferred(defer) && !collected.primary.is_empty() {
            collected.deferred.push(Deferred {
                label: defer_label.clone(),
                reversed_path: Vec::new(),
                selection_set: std::mem::take(&mut collected.primary),
                type_name: fragment_type.to_owned(),
            });
        }
        Ok(collected)
    }
}

impl Deferred {
    /// Wraps the deferred fragment's selections in a field it is nested in, and adds the field's
    /// response key to `reversed_path`. So this:
    /// { last }                 reversed_path []
    /// Becomes this:
    /// { name { last } }        reversed_path [name]
    fn wrap_in_field(
        mut self,
        name: &ByteString,
        alias: &Option<ByteString>,
        field_type: &FieldType,
    ) -> Self {
        self.reversed_path
            .push(alias.as_ref().unwrap_or(name).as_str().to_owned());
        self.selection_set = vec![Selection::Field {
            name: name.clone(),
            alias: alias.clone(),
            selection_set: Some(self.selection_set),
            field_type: field_type.clone(),
            include_skip: IncludeSkip::default(),
        }];
        self
    }

    /// Turns a deferred fragment that has reached the operation root into its subselection.
    fn into_subselection(
        self,
        defer_conditions: BooleanValues,
    ) -> (SubSelectionKey, SubSelectionValue) {
        let mut defer_path = self.reversed_path;
        // `reversed_path` is innermost first and `defer_path` starts at the
        // root, so we need to reverse
        defer_path.reverse();
        let key = SubSelectionKey {
            defer_label: self.label,
            defer_conditions,
            defer_path,
        };
        let value = SubSelectionValue {
            selection_set: self.selection_set,
            type_name: self.type_name,
        };
        (key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::BooleanValues;
    use super::SubSelectionKey;
    use super::collect_subselections;
    use crate::Configuration;
    use crate::spec::Query;
    use crate::spec::Schema;

    #[track_caller]
    fn assert_round_trips(key: SubSelectionKey) {
        let serialized = serde_json::to_string(&key).unwrap();
        let deserialized: SubSelectionKey = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, key, "did not round trip through {serialized}");
    }

    /// `SubSelectionKey` is serialized as a JSON object key when a `Query` goes through the
    /// distributed query plan cache, so it must round trip: a key that comes back different is a
    /// lookup miss, and a missed deferred subselection silently yields an empty incremental
    /// payload. `@defer(label:)` is an arbitrary string and may contain any number of the `|`
    /// field separator; path segments are response keys, which cannot.
    #[test]
    fn sub_selection_key_round_trips() {
        let cases = [
            (None, &[][..]),
            (Some("_UserFrag"), &["currentUser"][..]),
            (Some("0"), &["currentUser", "activeOrganization"][..]),
            (Some("_a|b"), &[][..]),
            (Some("_a|b"), &["a", "b"][..]),
            (Some("_a|b|c"), &["a", "b"][..]),
            (Some("_a|"), &["a"][..]),
        ];
        for (defer_label, defer_path) in cases {
            for bits in [0, 5] {
                assert_round_trips(SubSelectionKey {
                    defer_label: defer_label.map(str::to_owned),
                    defer_conditions: BooleanValues { bits },
                    defer_path: defer_path.iter().map(|s| (*s).to_owned()).collect(),
                });
            }
        }
    }

    /// Asserts a query has exactly one deferred subselection.
    #[track_caller]
    fn assert_deferred_type_name(query: &str, defer_path: &[&str], type_name: &str) {
        let configuration = Configuration::default();
        let schema = Schema::parse(
            include_str!("../../query_planner/testdata/schema.graphql"),
            &configuration,
        )
        .unwrap();
        let doc = Query::parse_document(query, None, &schema, &configuration).unwrap();
        let (fragments, operation, defer_stats, _) =
            Query::extract_query_information(&schema, query, &doc.executable, None).unwrap();
        let subselections =
            collect_subselections(&configuration, &operation, &fragments.map, &defer_stats)
                .unwrap();
        let deferred: Vec<_> = subselections
            .iter()
            .filter(|(key, _)| key.defer_label.is_some())
            .map(|(key, value)| {
                (
                    key.defer_label.as_deref(),
                    key.defer_path
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    value.type_name.as_str(),
                )
            })
            .collect();
        assert_eq!(deferred, [(Some("A"), defer_path.to_vec(), type_name)]);
    }

    #[test]
    fn deferred_type_name_under_type_condition() {
        assert_deferred_type_name(
            r#"{ topProducts { upc ... on Book { ... @defer(label: "A") { title } } } }"#,
            &["topProducts"],
            "Book",
        );
    }

    #[test]
    fn deferred_type_name_on_type_condition() {
        assert_deferred_type_name(
            r#"{ topProducts { upc ... on Book @defer(label: "A") { title } } }"#,
            &["topProducts"],
            "Book",
        );
    }

    #[test]
    fn deferred_type_name_of_fragment_spread() {
        assert_deferred_type_name(
            r#"{ topProducts { upc ...BookTitle @defer(label: "A") } }
            fragment BookTitle on Book { title }"#,
            &["topProducts"],
            "Book",
        );
    }

    #[test]
    fn deferred_type_name_under_type_condition_above_nested_field() {
        assert_deferred_type_name(
            r#"{ topProducts { upc ... on Book { reviews { id ... @defer(label: "A") { body } } } } }"#,
            &["topProducts", "reviews"],
            "Review",
        );
    }

    #[test]
    fn deferred_type_name_under_sibling_type_conditions() {
        assert_deferred_type_name(
            r#"{ topProducts { upc ... on Book { ...Deferred } ... on Furniture { ...Deferred } } }
            fragment Deferred on Product { ... @defer(label: "A") { name } }"#,
            &["topProducts"],
            "Product",
        );
    }
}
