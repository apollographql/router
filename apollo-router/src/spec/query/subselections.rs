use std::collections::HashMap;

use serde::de::Visitor;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;

use super::DeferStats;
use super::Operation;
use crate::spec::selection::Selection;
use crate::spec::Condition;
use crate::spec::FieldType;
use crate::spec::Fragment;
use crate::spec::IncludeSkip;
use crate::spec::SpecError;
use crate::Configuration;

#[derive(Debug, PartialEq, Hash, Eq)]
pub(crate) struct SubSelectionKey {
    pub(crate) defer_label: Option<String>,
    pub(crate) defer_conditions: BooleanValues,
}

// Do not replace this with a derived Serialize implementation
// SubSelectionKey must serialize to a string because it is used as a JSON object key
impl Serialize for SubSelectionKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = format!(
            "{:?}|{}",
            self.defer_conditions.bits,
            self.defer_label.as_deref().unwrap_or("")
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
impl<'de> Visitor<'de> for SubSelectionKeyVisitor {
    type Value = SubSelectionKey;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter
            .write_str("a string containing the defer label and defer conditions separated by |")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if let Some((bits_str, label)) = s.split_once('|') {
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
    operations: &[Operation],
    fragments: &HashMap<String, Fragment>,
    defer_stats: &DeferStats,
) -> Result<HashMap<SubSelectionKey, SubSelectionValue>, SpecError> {
    if !configuration.supergraph.defer_support || !defer_stats.has_defer {
        return Ok(HashMap::new());
    }
    if defer_stats.conditional_defer_variable_names.len() > MAX_DEFER_VARIABLES {
        // TODO: dedicated error variant?
        return Err(SpecError::ParsingError(
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
        for operation in operations {
            let type_name = operation.type_name.clone();
            let primary = collect_from_selection_set(
                &mut shared,
                &FieldType::new_named(&type_name),
                &operation.selection_set,
            )
            .map_err(|err| SpecError::ParsingError(err.to_owned()))?;
            debug_assert!(shared.path.is_empty());
            if !primary.is_empty() {
                shared.subselections.insert(
                    SubSelectionKey {
                        defer_label: None,
                        defer_conditions,
                    },
                    SubSelectionValue {
                        selection_set: primary,
                        type_name,
                    },
                );
            }
        }
    }
    Ok(shared.subselections)
}

/// Returns an iterator of functions, one per combination of boolean values of the given variables.
/// The function return whether a given variable (by its name) is true in that combination.
fn variable_combinations(defer_stats: &DeferStats) -> impl Iterator<Item = BooleanValues> {
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

/// Common arguments to multiple function calls
struct Shared<'a> {
    defer_stats: &'a DeferStats,
    fragments: &'a HashMap<String, Fragment>,
    defer_conditions: BooleanValues,
    path: Vec<(&'a ByteString, &'a FieldType)>,
    subselections: HashMap<SubSelectionKey, SubSelectionValue>,
}

impl Shared<'_> {
    fn eval_condition(&self, condition: &Condition) -> bool {
        match condition {
            Condition::Yes => true,
            Condition::No => false,
            Condition::Variable(name) => self.defer_conditions.eval(name, self.defer_stats),
        }
    }

    /// Take a selection set at `self.path` and reconstruct a selection set that belong
    /// at the root.
    fn reconstruct_up_to_root(&self, mut selection_set: Vec<Selection>) -> Vec<Selection> {
        for &(path_name, path_type) in self.path.iter().rev() {
            selection_set = vec![Selection::Field {
                name: path_name.clone(),
                alias: None,
                selection_set: Some(selection_set),
                field_type: path_type.clone(),
                include_skip: IncludeSkip::parse(&[]),
            }];
        }
        selection_set
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
                    shared.path.push((path_name, field_type));
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
                        new = FieldType::new_named(name);
                        &new
                    }
                    None => parent_type,
                };
                let nested = collect_from_selection_set(shared, fragment_type, nested)?;
                if nested.is_empty() {
                    // Every selection inside this inline fragment was deferred,
                    // so skip the inline fragment entirely from outer subselections.
                    continue;
                }
                let is_deferred = shared.eval_condition(defer);
                if is_deferred {
                    shared.subselections.insert(
                        SubSelectionKey {
                            defer_label: defer_label.clone(),
                            defer_conditions: shared.defer_conditions,
                        },
                        SubSelectionValue {
                            selection_set: shared.reconstruct_up_to_root(nested),
                            type_name: fragment_type.0.name(),
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
                let nested = collect_from_selection_set(
                    shared,
                    &FieldType::new_named(&fragment_definition.type_condition),
                    &fragment_definition.selection_set,
                )?;
                if nested.is_empty() {
                    // Every selection inside this fragment was deferred,
                    // so skip the fragment spread entirely from outer subselections.
                    continue;
                }
                let is_deferred = shared.eval_condition(defer);
                if is_deferred {
                    shared.subselections.insert(
                        SubSelectionKey {
                            defer_label: defer_label.clone(),
                            defer_conditions: shared.defer_conditions,
                        },
                        SubSelectionValue {
                            selection_set: shared.reconstruct_up_to_root(nested),
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
