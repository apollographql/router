use crate::error::FederationError;
use crate::query_graph::graph_path::selection_of_element;
use crate::query_graph::graph_path::OpPathElement;
use apollo_compiler::ast::Directive;
use apollo_compiler::executable::DirectiveList;
use apollo_compiler::executable::Name;
use apollo_compiler::executable::Value;
use indexmap::map::Entry;
use indexmap::IndexMap;
use std::sync::Arc;

use super::operation::normalized_selection_map::NormalizedSelectionMap;
use super::operation::NormalizedSelectionSet;

/// This struct is meant for tracking whether a selection set in a `FetchDependencyGraphNode` needs
/// to be queried, based on the `@skip`/`@include` applications on the selections within.
/// Accordingly, there is much logic around merging and short-circuiting; `OperationConditional` is
/// the more appropriate struct when trying to record the original structure/intent of those
/// `@skip`/`@include` applications.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Conditions {
    Variables(VariableConditions),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Condition {
    Variable(VariableCondition),
    Boolean(bool),
}

/// A list of variable conditions, represented as a map from variable names to whether that variable
/// is negated in the condition. We maintain the invariant that there's at least one condition (i.e.
/// the map is non-empty), and that there's at most one condition per variable name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableConditions(pub(crate) Arc<IndexMap<Name, bool>>);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableCondition {
    variable: Name,
    negated: bool,
}

impl Conditions {
    pub(crate) fn from_directives(directives: &DirectiveList) -> Result<Self, FederationError> {
        let mut variables = None;
        for directive in directives {
            let negated = match directive.name.as_str() {
                "include" => false,
                "skip" => true,
                _ => continue,
            };
            let value = directive.argument_by_name("if").ok_or_else(|| {
                FederationError::internal(format!(
                    "missing if argument on @{}",
                    if negated { "skip" } else { "include" },
                ))
            })?;
            match &**value {
                Value::Boolean(false) if !negated => return Ok(Self::Boolean(false)),
                Value::Boolean(true) if negated => return Ok(Self::Boolean(false)),
                Value::Boolean(_) => {}
                Value::Variable(name) => {
                    match variables
                        .get_or_insert_with(IndexMap::new)
                        .entry(name.clone())
                    {
                        Entry::Occupied(entry) => {
                            let previous_negated = *entry.get();
                            if previous_negated != negated {
                                return Ok(Self::Boolean(false));
                            }
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(negated);
                        }
                    }
                }
                _ => {
                    return Err(FederationError::internal(format!(
                        "expected boolean or variable `if` argument, got {value}",
                    )))
                }
            }
        }
        Ok(match variables {
            Some(map) => Self::Variables(VariableConditions(Arc::new(map))),
            None => Self::Boolean(true),
        })
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        match (self, other) {
            // Absorbing element
            (Conditions::Boolean(false), _) | (_, Conditions::Boolean(false)) => {
                Conditions::Boolean(false)
            }

            // Neutral element
            (Conditions::Boolean(true), x) | (x, Conditions::Boolean(true)) => x,

            (Conditions::Variables(mut self_vars), Conditions::Variables(other_vars)) => {
                let vars = Arc::make_mut(&mut self_vars.0);
                for (name, other_negated) in other_vars.0.iter() {
                    match vars.entry(name.clone()) {
                        Entry::Occupied(entry) => {
                            let self_negated = entry.get();
                            if self_negated != other_negated {
                                return Conditions::Boolean(false);
                            }
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(*other_negated);
                        }
                    }
                }
                Conditions::Variables(self_vars)
            }
        }
    }
}

fn is_constant_condition(condition: &Conditions) -> bool {
    match condition {
        Conditions::Variables(_) => false,
        Conditions::Boolean(_) => true,
    }
}

pub(crate) fn remove_conditions_from_selection_set(
    selection_set: &NormalizedSelectionSet,
    conditions: &Conditions,
) -> Result<NormalizedSelectionSet, FederationError> {
    match conditions {
        Conditions::Boolean(_) => {
            // If the conditions are the constant false, this means we know the selection will not be included
            // in the plan in practice, and it doesn't matter too much what we return here. So we just
            // the input unchanged as a shortcut.
            // If the conditions are the constant true, then it means we have no conditions to remove and we can
            // keep the selection "as is".
            Ok(selection_set.clone())
        }
        Conditions::Variables(variable_conditions) => {
            let mut selection_map = NormalizedSelectionMap::new();

            for selection in selection_set.selections.values() {
                let element = selection.element()?;
                // We remove any of the conditions on the element and recurse.
                let updated_element =
                    remove_conditions_of_element(element.clone(), variable_conditions);
                let new_selection = if let Ok(Some(selection_set)) = selection.selection_set() {
                    let updated_selection_set =
                        remove_conditions_from_selection_set(selection_set, conditions)?;
                    if updated_element == element {
                        if *selection_set == updated_selection_set {
                            selection.clone()
                        } else {
                            selection.with_updated_selection_set(Some(updated_selection_set))?
                        }
                    } else {
                        selection_of_element(updated_element, Some(updated_selection_set))?
                    }
                } else if updated_element == element {
                    selection.clone()
                } else {
                    selection_of_element(updated_element, None)?
                };
                selection_map.insert(new_selection);
            }

            Ok(NormalizedSelectionSet {
                schema: selection_set.schema.clone(),
                type_position: selection_set.type_position.clone(),
                selections: Arc::new(selection_map),
            })
        }
    }
}

fn remove_conditions_of_element(
    element: OpPathElement,
    conditions: &VariableConditions,
) -> OpPathElement {
    let updated_directives: DirectiveList = DirectiveList(
        element
            .directives()
            .iter()
            .filter(|d| {
                !matches_condition_for_kind(d, conditions, ConditionKind::Include)
                    && !matches_condition_for_kind(d, conditions, ConditionKind::Skip)
            })
            .cloned()
            .collect(),
    );

    if updated_directives.0.len() == element.directives().len() {
        element
    } else {
        element.with_updated_directives(updated_directives)
    }
}

#[derive(PartialEq)]
enum ConditionKind {
    Include,
    Skip,
}

fn matches_condition_for_kind(
    directive: &Directive,
    conditions: &VariableConditions,
    kind: ConditionKind,
) -> bool {
    let kind_str = match kind {
        ConditionKind::Include => "include",
        ConditionKind::Skip => "skip",
    };

    if directive.name != kind_str {
        return false;
    }

    let value = directive.argument_by_name("if");

    let matches_if_negated = match kind {
        ConditionKind::Include => false,
        ConditionKind::Skip => true,
    };
    match value {
        None => false,
        Some(v) => match v.as_variable() {
            Some(directive_var) => conditions.0.iter().any(|(cond_name, cond_is_negated)| {
                cond_name == directive_var && *cond_is_negated == matches_if_negated
            }),
            None => true,
        },
    }
}
