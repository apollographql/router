use std::sync::Arc;

use apollo_compiler::ast::Directive;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::Value;
use apollo_compiler::Name;
use apollo_compiler::Node;
use indexmap::map::Entry;
use serde::Serialize;

use crate::error::FederationError;
use crate::internal_error;
use crate::operation::DirectiveList;
use crate::operation::NamedFragments;
use crate::operation::Selection;
use crate::operation::SelectionMap;
use crate::operation::SelectionMapperReturn;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::OpPathElement;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ConditionKind {
    /// A `@skip(if:)` condition.
    Skip,
    /// An `@include(if:)` condition.
    Include,
}

impl ConditionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Include => "include",
        }
    }
}

/// This struct is meant for tracking whether a selection set in a `FetchDependencyGraphNode` needs
/// to be queried, based on the `@skip`/`@include` applications on the selections within.
/// Accordingly, there is much logic around merging and short-circuiting; `OperationConditional` is
/// the more appropriate struct when trying to record the original structure/intent of those
/// `@skip`/`@include` applications.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) enum Conditions {
    Variables(VariableConditions),
    Boolean(bool),
}

/// A list of variable conditions, represented as a map from variable names to whether that variable
/// is negated in the condition. We maintain the invariant that there's at least one condition (i.e.
/// the map is non-empty), and that there's at most one condition per variable name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct VariableConditions(
    // TODO(@goto-bus-stop): does it really make sense for this to be an indexmap? we normally only
    // have 1 or 2. Can we ever get so many conditions on the same node that it makes sense to use
    // a map over a vec?
    Arc<IndexMap<Name, ConditionKind>>,
);

impl VariableConditions {
    /// Construct VariableConditions from a non-empty map of variable names.
    ///
    /// In release builds, this does not check if the map is empty.
    fn new_unchecked(map: IndexMap<Name, ConditionKind>) -> Self {
        debug_assert!(!map.is_empty());
        Self(Arc::new(map))
    }

    /// Returns the condition kind of a variable, or None if there is no condition for the variable name.
    fn condition_kind(&self, name: &str) -> Option<ConditionKind> {
        self.0.get(name).copied()
    }

    /// Iterate all variable conditions and their kinds.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Name, ConditionKind)> {
        self.0.iter().map(|(name, &kind)| (name, kind))
    }

    /// Merge with another set of variable conditions. If the conditions conflict, returns `None`.
    fn merge(mut self, other: Self) -> Option<Self> {
        let vars = Arc::make_mut(&mut self.0);
        for (name, other_kind) in other.0.iter() {
            match vars.entry(name.clone()) {
                // `@skip(if: $var)` and `@include(if: $var)` on the same selection always means
                // it's not included.
                Entry::Occupied(self_kind) if self_kind.get() != other_kind => {
                    return None;
                }
                Entry::Occupied(_entry) => {}
                Entry::Vacant(entry) => {
                    entry.insert(*other_kind);
                }
            }
        }
        Some(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableCondition {
    variable: Name,
    kind: ConditionKind,
}

impl Conditions {
    /// Create conditions from a map of variable conditions. If empty, instead returns a
    /// condition that always evaluates to true.
    fn from_variables(map: IndexMap<Name, ConditionKind>) -> Self {
        if map.is_empty() {
            Self::Boolean(true)
        } else {
            Self::Variables(VariableConditions::new_unchecked(map))
        }
    }

    /// Parse @skip and @include conditions from a directive list.
    pub(crate) fn from_directives(directives: &DirectiveList) -> Result<Self, FederationError> {
        let mut variables = IndexMap::default();

        if let Some(skip) = directives.get("skip") {
            let Some(value) = skip.specified_argument_by_name("if") else {
                internal_error!("missing @skip(if:) argument");
            };

            match value.as_ref() {
                // Constant @skip(if: true) can never match
                Value::Boolean(true) => return Ok(Self::Boolean(false)),
                // Constant @skip(if: false) always matches
                Value::Boolean(_) => {}
                Value::Variable(name) => {
                    variables.insert(name.clone(), ConditionKind::Skip);
                }
                _ => {
                    internal_error!("expected boolean or variable `if` argument, got {value}");
                }
            }
        }

        if let Some(include) = directives.get("include") {
            let Some(value) = include.specified_argument_by_name("if") else {
                internal_error!("missing @include(if:) argument");
            };

            match value.as_ref() {
                // Constant @include(if: false) can never match
                Value::Boolean(false) => return Ok(Self::Boolean(false)),
                // Constant @include(if: true) always matches
                Value::Boolean(true) => {}
                // If both @skip(if: $var) and @include(if: $var) exist, the condition can also
                // never match
                Value::Variable(name) => {
                    if variables.insert(name.clone(), ConditionKind::Include)
                        == Some(ConditionKind::Skip)
                    {
                        return Ok(Self::Boolean(false));
                    }
                }
                _ => {
                    internal_error!("expected boolean or variable `if` argument, got {value}");
                }
            }
        }

        Ok(Self::from_variables(variables))
    }

    // TODO(@goto-bus-stop): what exactly is the difference between this and `Self::merge`?
    pub(crate) fn update_with(&self, new_conditions: &Self) -> Self {
        match (new_conditions, self) {
            (Conditions::Boolean(_), _) | (_, Conditions::Boolean(_)) => new_conditions.clone(),
            (Conditions::Variables(new_conditions), Conditions::Variables(handled_conditions)) => {
                let mut filtered = IndexMap::default();
                for (cond_name, &cond_kind) in new_conditions.0.iter() {
                    match handled_conditions.condition_kind(cond_name) {
                        Some(handled_cond_kind) if cond_kind != handled_cond_kind => {
                            // If we've already handled that exact condition, we can skip it.
                            // But if we've already handled the _negation_ of this condition, then this mean the overall conditions
                            // are unreachable and we can just return `false` directly.
                            return Conditions::Boolean(false);
                        }
                        Some(_) => {}
                        None => {
                            filtered.insert(cond_name.clone(), cond_kind);
                        }
                    }
                }
                Self::from_variables(filtered)
            }
        }
    }

    /// Merge two sets of conditions. The new conditions evaluate to true only if both input
    /// conditions evaluate to true.
    pub(crate) fn merge(self, other: Self) -> Self {
        match (self, other) {
            // Absorbing element
            (Conditions::Boolean(false), _) | (_, Conditions::Boolean(false)) => {
                Conditions::Boolean(false)
            }

            // Neutral element
            (Conditions::Boolean(true), x) | (x, Conditions::Boolean(true)) => x,

            (Conditions::Variables(self_vars), Conditions::Variables(other_vars)) => {
                match self_vars.merge(other_vars) {
                    Some(vars) => Conditions::Variables(vars),
                    None => Conditions::Boolean(false),
                }
            }
        }
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
            selection_set.lazy_map(&NamedFragments::default(), |selection| {
                let element = selection.element()?;
                // We remove any of the conditions on the element and recurse.
                let updated_element =
                    remove_conditions_of_element(element.clone(), variable_conditions);
                if let Some(selection_set) = selection.selection_set() {
                    let updated_selection_set =
                        remove_conditions_from_selection_set(selection_set, conditions)?;
                    if updated_element == element {
                        if *selection_set == updated_selection_set {
                            Ok(SelectionMapperReturn::Selection(selection.clone()))
                        } else {
                            Ok(SelectionMapperReturn::Selection(
                                selection
                                    .with_updated_selection_set(Some(updated_selection_set))?,
                            ))
                        }
                    } else {
                        Ok(SelectionMapperReturn::Selection(Selection::from_element(
                            updated_element,
                            Some(updated_selection_set),
                        )?))
                    }
                } else if updated_element == element {
                    Ok(SelectionMapperReturn::Selection(selection.clone()))
                } else {
                    Ok(SelectionMapperReturn::Selection(Selection::from_element(
                        updated_element,
                        None,
                    )?))
                }
            })
        }
    }
}

/// Given a `selection_set` and given a set of directive applications that can be eliminated (`unneeded_directives`; in
/// practice those are conditionals (@skip and @include) already accounted for), returns an equivalent selection set but with unnecessary
/// "starting" fragments having the unneeded condition/directives removed.
pub(crate) fn remove_unneeded_top_level_fragment_directives(
    selection_set: &SelectionSet,
    unneeded_directives: &DirectiveList,
) -> Result<SelectionSet, FederationError> {
    let mut selection_map = SelectionMap::new();

    for selection in selection_set.selections.values() {
        match selection {
            Selection::Field(_) => {
                selection_map.insert(selection.clone());
            }
            Selection::InlineFragment(inline_fragment) => {
                let fragment = &inline_fragment.inline_fragment;
                if fragment.type_condition_position.is_none() {
                    // if there is no type condition we should preserve the directive info
                    selection_map.insert(selection.clone());
                } else {
                    let needed_directives: Vec<Node<Directive>> = fragment
                        .directives
                        .iter()
                        .filter(|directive| !unneeded_directives.contains(directive))
                        .cloned()
                        .collect();

                    // We recurse, knowing that we'll stop as soon as we hit field selections, so this only cover the fragments
                    // at the "top-level" of the set.
                    let updated_selections = remove_unneeded_top_level_fragment_directives(
                        &inline_fragment.selection_set,
                        unneeded_directives,
                    )?;
                    if needed_directives.len() == fragment.directives.len() {
                        // We need all the directives that the fragment has. Return it unchanged.
                        let final_selection =
                            inline_fragment.with_updated_selection_set(updated_selections);
                        selection_map.insert(Selection::InlineFragment(Arc::new(final_selection)));
                    } else {
                        // We can skip some of the fragment directives directive.
                        let final_selection = inline_fragment
                            .with_updated_directives_and_selection_set(
                                DirectiveList::from_iter(needed_directives),
                                updated_selections,
                            );
                        selection_map.insert(Selection::InlineFragment(Arc::new(final_selection)));
                    }
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
    let updated_directives: DirectiveList = element
        .directives()
        .iter()
        .filter(|d| {
            !matches_condition_for_kind(d, conditions, ConditionKind::Include)
                && !matches_condition_for_kind(d, conditions, ConditionKind::Skip)
        })
        .cloned()
        .collect();

    if updated_directives.len() == element.directives().len() {
        element
    } else {
        element.with_updated_directives(updated_directives)
    }
}

fn matches_condition_for_kind(
    directive: &Directive,
    conditions: &VariableConditions,
    kind: ConditionKind,
) -> bool {
    if directive.name != kind.as_str() {
        return false;
    }

    match directive.specified_argument_by_name("if") {
        Some(v) => match v.as_variable() {
            Some(directive_var) => conditions.condition_kind(directive_var) == Some(kind),
            None => true,
        },
        // Directive without argument: unreachable in a valid document.
        None => false,
    }
}
