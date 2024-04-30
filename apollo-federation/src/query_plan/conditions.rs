use std::sync::Arc;

use apollo_compiler::ast::Directive;
use apollo_compiler::executable::DirectiveList;
use apollo_compiler::executable::Name;
use apollo_compiler::executable::Value;
use apollo_compiler::Node;
use indexmap::map::Entry;
use indexmap::IndexMap;

use crate::error::FederationError;
use crate::query_graph::graph_path::OpPathElement;
use crate::query_plan::operation::Selection;
use crate::query_plan::operation::SelectionMap;
use crate::query_plan::operation::SelectionSet;

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
pub(crate) struct VariableConditions(Arc<IndexMap<Name, bool>>);

impl VariableConditions {
    /// Construct VariableConditions from a non-empty map of variable names.
    ///
    /// In release builds, this does not check if the map is empty.
    fn new_unchecked(map: IndexMap<Name, bool>) -> Self {
        debug_assert!(!map.is_empty());
        Self(Arc::new(map))
    }

    pub fn insert(&mut self, name: Name, negated: bool) {
        Arc::make_mut(&mut self.0).insert(name, negated);
    }

    /// Returns true if there are no conditions.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns a variable condition by name.
    pub fn get(&self, name: &str) -> Option<VariableCondition> {
        self.0.get_key_value(name).map(|(variable, &negated)| {
            let variable = variable.clone();
            VariableCondition { variable, negated }
        })
    }

    /// Returns whether a variable condition is negated, or None if there is no condition for the variable name.
    pub fn is_negated(&self, name: &str) -> Option<bool> {
        self.0.get(name).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Name, bool)> {
        self.0.iter().map(|(name, &negated)| (name, negated))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableCondition {
    variable: Name,
    negated: bool,
}

impl Conditions {
    /// Create conditions from a map of variable conditions. If empty, instead returns a
    /// condition that always evaluates to true.
    fn from_variables(map: IndexMap<Name, bool>) -> Self {
        if map.is_empty() {
            Self::Boolean(true)
        } else {
            Self::Variables(VariableConditions::new_unchecked(map))
        }
    }

    pub(crate) fn from_directives(directives: &DirectiveList) -> Result<Self, FederationError> {
        let mut variables = IndexMap::new();
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
                Value::Variable(name) => match variables.entry(name.clone()) {
                    Entry::Occupied(entry) => {
                        let previous_negated = *entry.get();
                        if previous_negated != negated {
                            return Ok(Self::Boolean(false));
                        }
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(negated);
                    }
                },
                _ => {
                    return Err(FederationError::internal(format!(
                        "expected boolean or variable `if` argument, got {value}",
                    )))
                }
            }
        }
        Ok(Self::from_variables(variables))
    }

    pub(crate) fn update_with(&self, new_conditions: &Self) -> Self {
        match (new_conditions, self) {
            (Conditions::Boolean(_), _) | (_, Conditions::Boolean(_)) => new_conditions.clone(),
            (Conditions::Variables(new_conditions), Conditions::Variables(handled_conditions)) => {
                let mut filtered = IndexMap::new();
                for (cond_name, &cond_negated) in new_conditions.0.iter() {
                    match handled_conditions.is_negated(cond_name) {
                        Some(handled_cond) if cond_negated != handled_cond => {
                            // If we've already handled that exact condition, we can skip it.
                            // But if we've already handled the _negation_ of this condition, then this mean the overall conditions
                            // are unreachable and we can just return `false` directly.
                            return Conditions::Boolean(false);
                        }
                        Some(_) => {}
                        None => {
                            filtered.insert(cond_name.clone(), cond_negated);
                        }
                    }
                }
                Self::from_variables(filtered)
            }
        }
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
    selection_set: &SelectionSet,
    conditions: &Conditions,
) -> Result<SelectionSet, FederationError> {
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
            let mut selection_map = SelectionMap::new();

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
                        Selection::from_element(updated_element, Some(updated_selection_set))?
                    }
                } else if updated_element == element {
                    selection.clone()
                } else {
                    Selection::from_element(updated_element, None)?
                };
                selection_map.insert(new_selection);
            }

            Ok(SelectionSet {
                schema: selection_set.schema.clone(),
                type_position: selection_set.type_position.clone(),
                selections: Arc::new(selection_map),
            })
        }
    }
}

/// Given a `selection_set` and given a set of directive applications that can be eliminated (`unneeded_directives`; in
/// practice those are conditionals (@skip and @include) already accounted for), returns an equivalent selection set but with unnecessary
/// "starting" fragments having the unneeded condition/directives removed.
pub(crate) fn remove_unneeded_top_level_fragment_directives(
    selection_set: &SelectionSet,
    unneded_directives: &DirectiveList,
) -> Result<SelectionSet, FederationError> {
    let mut selection_map = SelectionMap::new();

    for selection in selection_set.selections.values() {
        match selection {
            Selection::Field(_) => {
                selection_map.insert(selection.clone());
            }
            Selection::InlineFragment(inline_fragment) => {
                let fragment = inline_fragment.inline_fragment.data();
                if fragment.type_condition_position.is_none() {
                    // if there is no type condition we should preserve the directive info
                    selection_map.insert(selection.clone());
                } else {
                    let mut needed_directives: Vec<Node<Directive>> = Vec::new();
                    if fragment.directives.len() > 0 {
                        for directive in fragment.directives.iter() {
                            if !unneded_directives.contains(directive) {
                                needed_directives.push(directive.clone());
                            }
                        }
                    }

                    // We recurse, knowing that we'll stop as soon as we hit field selections, so this only cover the fragments
                    // at the "top-level" of the set.
                    let updated_selections = remove_unneeded_top_level_fragment_directives(
                        &inline_fragment.selection_set,
                        unneded_directives,
                    )?;
                    if needed_directives.len() == fragment.directives.len() {
                        // We need all the directives that the fragment has. Return it unchanged.
                        let final_selection =
                            inline_fragment.with_updated_selection_set(Some(updated_selections));
                        selection_map.insert(Selection::InlineFragment(Arc::new(final_selection)));
                    }

                    // We can skip some of the fragment directives directive.
                    let final_selection =
                        inline_fragment.with_updated_directives(DirectiveList(needed_directives));
                    selection_map.insert(Selection::InlineFragment(Arc::new(final_selection)));
                }
            }
            _ => {
                // TODO should we apply same logic as for inline_fragment "just in case"?
                return Err(FederationError::internal("unexpected fragment spread"));
            }
        }
    }

    Ok(SelectionSet {
        schema: selection_set.schema.clone(),
        type_position: selection_set.type_position.clone(),
        selections: Arc::new(selection_map),
    })
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
