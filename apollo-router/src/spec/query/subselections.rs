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

/// The values of boolean variables used for conditional `@defer`, packed least-significant-bit
/// first in iteration order of `DeferStats::conditional_defer_variable_names`.
#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Hash, Eq)]
#[serde(transparent)]
pub(crate) struct BooleanValues {
    pub(crate) bits: u32,
}

pub(crate) const DEFER_DIRECTIVE_NAME: &str = "defer";

/// We generate subselections for all 2^N possible combinations of these boolean variables.
/// Refuse to do so for a number of combinatinons we deem unreasonable.
const MAX_DEFER_VARIABLES: usize = 4;

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
        // TODO: dedicated error variant?
        return Err(SpecError::TransformError(
            "@defer conditional on too many different variables".into(),
        ));
    }
    let mut shared = Shared {
        defer_stats,
        fragments,
        defer_conditions: BooleanValues { bits: 0 }, // overwritten
        path: Vec::new(),
        subselections: HashMap::new(),
    };
    for defer_conditions in variable_combinations(defer_stats) {
        shared.defer_conditions = defer_conditions;
        let type_name = operation.type_name.clone();
        let primary = collect_from_selection_set(
            &mut shared,
            // FIXME: use `ast::Name` everywhere so fallible conversion isn’t needed
            #[allow(clippy::unwrap_used)]
            &FieldType::new_named((&type_name).try_into().unwrap()),
            &operation.selection_set,
        )
        .map_err(|err| SpecError::TransformError(err.to_owned()))?;
        debug_assert!(shared.path.is_empty());
        if !primary.is_empty() {
            shared.subselections.insert(
                SubSelectionKey {
                    defer_label: None,
                    defer_conditions,
                    defer_path: Vec::new(),
                },
                SubSelectionValue {
                    selection_set: primary,
                    type_name,
                },
            );
        }
    }
    Ok(shared.subselections)
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

/// Common arguments to multiple function calls
struct Shared<'a> {
    defer_stats: &'a DeferStats,
    fragments: &'a HashMap<String, Fragment>,
    defer_conditions: BooleanValues,
    path: Vec<PathSegment<'a>>,
    subselections: HashMap<SubSelectionKey, SubSelectionValue>,
}

/// A step from the root to the current selection set.
enum PathSegment<'a> {
    Field {
        name: &'a ByteString,
        field_type: &'a FieldType,
    },
    Fragment {
        type_condition: &'a str,
        known_type: &'a Option<String>,
    },
}

impl<'a> Shared<'a> {
    fn eval_condition(&self, condition: &Condition) -> bool {
        match condition {
            Condition::Yes => true,
            Condition::No => false,
            Condition::Variable(name) => self.defer_conditions.eval(name, self.defer_stats),
        }
    }

    fn defer_path(&self) -> Vec<String> {
        self.path
            .iter()
            .filter_map(|segment| match segment {
                PathSegment::Field { name, .. } => Some(name.as_str().to_owned()),
                PathSegment::Fragment { .. } => None,
            })
            .collect()
    }

    /// Run `f` with the fragment's type condition on `self.path`.
    ///
    /// Only fragments that narrow `parent_type` are recorded, and not at the root, where the
    /// type is always the operation type.
    ///
    /// The formatter applies a narrowing fragment only when the data has `__typename`, so this
    /// relies on the planner fetching `__typename` wherever a deferred fetch narrows the type.
    fn in_fragment<R>(
        &mut self,
        parent_type: &FieldType,
        type_condition: &'a str,
        known_type: &'a Option<String>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        if self.path.is_empty() || parent_type.0.inner_named_type().as_str() == type_condition {
            return f(self);
        }
        self.path.push(PathSegment::Fragment {
            type_condition,
            known_type,
        });
        let result = f(self);
        self.path.pop();
        result
    }

    /// Take a selection set at `self.path` and reconstruct a selection set that belong
    /// at the root.
    ///
    /// Type conditions on the path are kept so that formatting a deferred response does not
    /// apply the deferred fields to objects of other types, such as siblings in a list.
    fn reconstruct_up_to_root(&self, mut selection_set: Vec<Selection>) -> Vec<Selection> {
        for segment in self.path.iter().rev() {
            selection_set = vec![match *segment {
                PathSegment::Field { name, field_type } => Selection::Field {
                    name: name.clone(),
                    alias: None,
                    selection_set: Some(selection_set),
                    field_type: field_type.clone(),
                    include_skip: IncludeSkip::default(),
                },
                PathSegment::Fragment {
                    type_condition,
                    known_type,
                } => Selection::InlineFragment {
                    type_condition: type_condition.to_owned(),
                    include_skip: IncludeSkip::default(),
                    defer: Condition::No,
                    defer_label: None,
                    known_type: known_type.clone(),
                    selection_set,
                },
            }];
        }
        selection_set
    }

    /// A labeled `@defer` in a fragment spread under several type conditions, such as
    /// `... on A { ...F } ... on B { ...F }`, yields one key for several reconstructed selection
    /// sets. Merge them so each type condition is kept.
    ///
    /// The query planner labels every `@defer`, so only a fragment spread repeats a label.
    fn insert_subselection(&mut self, key: SubSelectionKey, value: SubSelectionValue) {
        match self.subselections.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            // Both values come from the same `@defer`, so they have the same `type_name`.
            Entry::Occupied(mut entry) => {
                merge_selection_sets(&mut entry.get_mut().selection_set, value.selection_set);
            }
        }
    }
}

/// Merge `extra` into `target`, combining fields with the same response key and inline
/// fragments with the same type condition, so the formatter sees one tree.
fn merge_selection_sets(target: &mut Vec<Selection>, extra: Vec<Selection>) {
    for selection in extra {
        let position = target
            .iter()
            .position(|existing| same_response_position(existing, &selection));
        match position {
            None => target.push(selection),
            Some(i) => match (&mut target[i], selection) {
                (
                    Selection::Field {
                        selection_set: Some(existing),
                        ..
                    },
                    Selection::Field {
                        selection_set: Some(extra),
                        ..
                    },
                ) => merge_selection_sets(existing, extra),
                (
                    Selection::InlineFragment {
                        selection_set: existing,
                        ..
                    },
                    Selection::InlineFragment {
                        selection_set: extra,
                        ..
                    },
                ) => merge_selection_sets(existing, extra),
                // A leaf field that is already present.
                _ => {}
            },
        }
    }
}

fn same_response_position(a: &Selection, b: &Selection) -> bool {
    match (a, b) {
        (
            Selection::Field {
                name: n1,
                alias: a1,
                include_skip: s1,
                ..
            },
            Selection::Field {
                name: n2,
                alias: a2,
                include_skip: s2,
                ..
            },
        ) => a1.as_ref().unwrap_or(n1) == a2.as_ref().unwrap_or(n2) && s1 == s2,
        (
            Selection::InlineFragment {
                type_condition: t1,
                include_skip: s1,
                defer: d1,
                ..
            },
            Selection::InlineFragment {
                type_condition: t2,
                include_skip: s2,
                defer: d2,
                ..
            },
        ) => t1 == t2 && s1 == s2 && d1 == d2,
        _ => false,
    }
}

/// Insert deferred subselections in `subselections` and return non-defered parts.
fn collect_from_selection_set<'a>(
    shared: &mut Shared<'a>,
    parent_type: &FieldType,
    selection_set: &'a [Selection],
) -> Result<Vec<Selection>, &'static str> {
    let mut primary = Vec::new();
    for selection in selection_set {
        match selection {
            Selection::Field {
                name,
                alias,
                selection_set: nested,
                field_type,
                include_skip,
            } => {
                let primary_nested = if let Some(nested) = nested {
                    let path_name = alias.as_ref().unwrap_or(name);
                    shared.path.push(PathSegment::Field {
                        name: path_name,
                        field_type,
                    });
                    let collected = collect_from_selection_set(shared, field_type, nested)?;
                    shared.path.pop();
                    if !collected.is_empty() {
                        Some(collected)
                    } else {
                        // Every nested selection of this field is deferred,
                        // so skip this field entirely in the primary subselection.
                        continue;
                    }
                } else {
                    None
                };
                primary.push(Selection::Field {
                    selection_set: primary_nested,
                    name: name.clone(),
                    alias: alias.clone(),
                    field_type: field_type.clone(),
                    include_skip: include_skip.clone(),
                })
            }
            Selection::InlineFragment {
                type_condition,
                include_skip,
                defer,
                defer_label,
                known_type,
                selection_set: nested,
            } => {
                let new;
                let fragment_type = match known_type {
                    Some(name) => {
                        // FIXME: use `ast::Name` everywhere so fallible conversion isn’t needed
                        #[allow(clippy::unwrap_used)]
                        {
                            new = FieldType::new_named(name.try_into().unwrap());
                        }
                        &new
                    }
                    None => parent_type,
                };
                let nested =
                    shared.in_fragment(parent_type, type_condition, known_type, |shared| {
                        collect_from_selection_set(shared, fragment_type, nested)
                    })?;
                if nested.is_empty() {
                    // Every selection inside this inline fragment was deferred,
                    // so skip the inline fragment entirely from outer subselections.
                    continue;
                }
                let is_deferred = shared.eval_condition(defer);
                if is_deferred {
                    // Wrap in the fragment again: the first call popped it.
                    let selection_set =
                        shared.in_fragment(parent_type, type_condition, known_type, |shared| {
                            shared.reconstruct_up_to_root(nested)
                        });
                    shared.insert_subselection(
                        SubSelectionKey {
                            defer_label: defer_label.clone(),
                            defer_conditions: shared.defer_conditions,
                            defer_path: shared.defer_path(),
                        },
                        SubSelectionValue {
                            selection_set,
                            type_name: fragment_type.0.inner_named_type().as_str().to_owned(),
                        },
                    );
                } else {
                    primary.push(Selection::InlineFragment {
                        type_condition: type_condition.clone(),
                        include_skip: include_skip.clone(),
                        defer: defer.clone(),
                        defer_label: defer_label.clone(),
                        known_type: known_type.clone(),
                        selection_set: nested,
                    })
                }
            }
            Selection::FragmentSpread {
                name,
                known_type,
                include_skip,
                defer,
                defer_label,
            } => {
                let fragment_definition = shared
                    .fragments
                    .get(name)
                    .ok_or("Missing fragment definition")?;
                let type_condition = &fragment_definition.type_condition;
                let nested =
                    shared.in_fragment(parent_type, type_condition, known_type, |shared| {
                        collect_from_selection_set(
                            shared,
                            // FIXME: use `ast::Name` everywhere so fallible conversion isn’t needed
                            #[allow(clippy::unwrap_used)]
                            &FieldType::new_named(type_condition.try_into().unwrap()),
                            &fragment_definition.selection_set,
                        )
                    })?;
                if nested.is_empty() {
                    // Every selection inside this fragment was deferred,
                    // so skip the fragment spread entirely from outer subselections.
                    continue;
                }
                let is_deferred = shared.eval_condition(defer);
                if is_deferred {
                    // Wrap in the fragment again: the first call popped it.
                    let selection_set =
                        shared.in_fragment(parent_type, type_condition, known_type, |shared| {
                            shared.reconstruct_up_to_root(nested)
                        });
                    shared.insert_subselection(
                        SubSelectionKey {
                            defer_label: defer_label.clone(),
                            defer_conditions: shared.defer_conditions,
                            defer_path: shared.defer_path(),
                        },
                        SubSelectionValue {
                            selection_set,
                            type_name: fragment_definition.type_condition.clone(),
                        },
                    );
                } else {
                    // Convert the fragment spread to an inline fragment
                    // so that subselections are self-contained
                    primary.push(Selection::InlineFragment {
                        type_condition: fragment_definition.type_condition.clone(),
                        include_skip: include_skip.clone(),
                        defer: defer.clone(),
                        defer_label: defer_label.clone(),
                        known_type: known_type.clone(),
                        selection_set: nested,
                    })
                }
            }
        }
    }
    Ok(primary)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::BooleanValues;
    use super::SubSelectionKey;
    use super::merge_selection_sets;
    use crate::spec::Condition;
    use crate::spec::FieldType;
    use crate::spec::IncludeSkip;
    use crate::spec::selection::Selection;

    fn field(name: &str, selection_set: Option<Vec<Selection>>) -> Selection {
        Selection::Field {
            name: name.into(),
            alias: None,
            selection_set,
            field_type: FieldType::new_named(name!("T")),
            include_skip: IncludeSkip::default(),
        }
    }

    fn inline_fragment(type_condition: &str, selection_set: Vec<Selection>) -> Selection {
        Selection::InlineFragment {
            type_condition: type_condition.to_owned(),
            include_skip: IncludeSkip::default(),
            defer: Condition::No,
            defer_label: None,
            known_type: Some(type_condition.to_owned()),
            selection_set,
        }
    }

    /// `page { mixed { children { ... on <type_condition> { <leaf> } } } }`
    fn under_children(type_condition: &str, leaf: &str) -> Vec<Selection> {
        vec![field(
            "page",
            Some(vec![field(
                "mixed",
                Some(vec![field(
                    "children",
                    Some(vec![inline_fragment(
                        type_condition,
                        vec![field(leaf, None)],
                    )]),
                )]),
            )]),
        )]
    }

    #[test]
    fn merge_selection_sets_keeps_each_type_condition() {
        let mut merged = under_children("Item", "title");
        merge_selection_sets(&mut merged, under_children("Other", "label"));
        assert_eq!(
            merged,
            vec![field(
                "page",
                Some(vec![field(
                    "mixed",
                    Some(vec![field(
                        "children",
                        Some(vec![
                            inline_fragment("Item", vec![field("title", None)]),
                            inline_fragment("Other", vec![field("label", None)]),
                        ]),
                    )]),
                )]),
            )]
        );
    }

    #[test]
    fn merge_selection_sets_into_itself_is_unchanged() {
        let mut merged = under_children("Item", "title");
        merge_selection_sets(&mut merged, under_children("Item", "title"));
        assert_eq!(merged, under_children("Item", "title"));
    }

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
}
