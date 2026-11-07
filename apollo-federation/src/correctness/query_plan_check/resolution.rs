//! What a fetch's subgraph can resolve.
//!
//! Port of the resolution section of `Apollo/Definitions/QueryPlanSoundness.lean` (`FetchResolves`,
//! `SelectionsResolved`, `ProvidesWindow`, `windowEntries`, `WindowGrants`, `windowBelow`) and of
//! its decider in `Apollo/Implementations/QueryPlanChecker.lean` (`fetchResolvesBool`,
//! `selectionsResolvedBool`, `ResolutionState`, `resolutionFieldStates`).
//!
//! A fetch sends its subgraph an operation, which the subgraph answers only for the fields it can
//! resolve. A window is the position-dependent permission a subgraph's `@provides` field set grants
//! to resolve the external fields it names. Every fetch owes `FetchResolves`, root fetches
//! included: walking the operation from its root type, every field is declared on the runtime type
//! it is selected on, the subgraph can resolve it there, outright or inside the window open at that
//! position, and it can resolve what is selected below it at every runtime type the field's result
//! can have.
//!
//! - **Resolved outright.** A subgraph can resolve a field unless its metadata reads the field as
//!   external ([`Subgraph::resolves`]): a field marked `@external`, or a field of a type marked
//!   `@external`. It can resolve a fake external, a key field of a type extension marked
//!   `@external`.
//! - **Inside a window.** A window opens at the result of the field declaring the `@provides`, for
//!   the runtime type whose field declares it, and grants what its field set names under the field
//!   set's type conditions; a window opened inside another adds to it, and nothing a sibling field
//!   grants descends.
//! - **`__typename`** needs no declaration: every subgraph can resolve it, everywhere.
//! - **No key exemption.** A representation's members travel in the `representations` variable and
//!   are never selected, so a key field the operation selects under `_entities` is one it asks the
//!   subgraph to resolve, like any other.
//!
//! The judgement is per runtime type: a field spelled on an interface is a field of every member a
//! position can hold, and one member can mark it `@external` where another resolves it. Everything
//! is read in the subgraph's own namespace, where the operation and every `@provides` field set are
//! parsed. The walk descends each selection once, with every state a position can be in: a runtime
//! type with the window open on it. The states are pooled, a type reached twice with one window
//! being one state and a type reached with two windows two, so no type is credited with another's
//! window, and a chain through an interface with several implementations is asked once per level
//! rather than once per runtime path.
//!
//! The check validates no fetch operation as GraphQL and establishes nothing beyond the obligation
//! it states: a field the subgraph does not declare is not one it can resolve.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;

use super::ComparisonError;
use super::display_path;
use super::selections::admitted_types;
use super::selections::possible_types;
use super::subgraph::Subgraph;
use crate::query_plan::FetchDataPathElement;
use crate::schema::ValidFederationSchema;
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;

/// Can a fetch's subgraph resolve everything the fetch's operation selects? Decides
/// `FetchResolves`: the walk starts at the operation's root type, with no window open there.
///
/// `operation` is the fetch's own operation, its fragment spreads inlined. `subgraph_name` and
/// `path`, where the fetch runs, are for the explanation.
pub(super) fn check_fetch_resolves(
    subgraph: &Subgraph<'_>,
    subgraph_name: &str,
    path: &[FetchDataPathElement],
    operation: &executable::SelectionSet,
) -> Result<(), ComparisonError> {
    let mut walk = ResolutionWalk {
        subgraph,
        subgraph_name,
        path,
        selected_at: Vec::new(),
    };
    let root = ResolutionState {
        object_type: operation.ty.clone(),
        window: Vec::new(),
    };
    walk.selections_resolved(&[root], &operation.selections)
}

/// A runtime type with the window open on it: the model's `ResolutionState`.
///
/// The window is the parsed `@provides` selection set itself, written at the position; the model
/// projects it to a `ProvidesWindow` only to have decidable equality, which a selection set has
/// here.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ResolutionState {
    object_type: Name,
    window: Vec<Selection>,
}

struct ResolutionWalk<'a> {
    subgraph: &'a Subgraph<'a>,
    subgraph_name: &'a str,
    path: &'a [FetchDataPathElement],
    /// The response keys from the operation's root to the field being judged, for the
    /// explanation.
    selected_at: Vec<Name>,
}

impl ResolutionWalk<'_> {
    /// Decides `SelectionsResolved` at every state of a pool at once, walking each selection once:
    /// the model's `selectionsResolvedBool`.
    fn selections_resolved(
        &mut self,
        states: &[ResolutionState],
        selections: &[Selection],
    ) -> Result<(), ComparisonError> {
        for selection in selections {
            match selection {
                Selection::Field(field) => {
                    if field.name == *INTROSPECTION_TYPENAME_FIELD_NAME {
                        continue;
                    }
                    self.selected_at.push(field.response_name().clone());
                    for state in states {
                        self.field_resolved(state, &field.name)?;
                    }
                    if !field.selection_set.selections.is_empty() {
                        let below = field_states(self.subgraph, &field.name, states)?;
                        self.selections_resolved(&below, &field.selection_set.selections)?;
                    }
                    self.selected_at.pop();
                }
                Selection::InlineFragment(fragment) => {
                    let admitted =
                        admitted_types(self.subgraph.schema(), fragment.type_condition.as_slice());
                    let inside: Vec<ResolutionState> = states
                        .iter()
                        .filter(|state| {
                            admitted
                                .as_ref()
                                .is_none_or(|types| types.contains(&state.object_type))
                        })
                        .cloned()
                        .collect();
                    self.selections_resolved(&inside, &fragment.selection_set.selections)?;
                }
                // Spreads are inlined before a fetch's operation reaches here.
                Selection::FragmentSpread(_) => {}
            }
        }
        Ok(())
    }

    /// Whether the subgraph can resolve a field at one state: the field is declared on the state's
    /// runtime type, and resolved there outright or granted by the window open on it.
    fn field_resolved(
        &self,
        state: &ResolutionState,
        field_name: &Name,
    ) -> Result<(), ComparisonError> {
        let object_type = &state.object_type;
        let reason = if self.subgraph.field(object_type, field_name).is_none() {
            "is not a field the subgraph declares"
        } else if self.subgraph.resolves(object_type, field_name)
            || window_grants(
                self.subgraph.schema(),
                &state.window,
                object_type,
                field_name,
            )
        {
            return Ok(());
        } else {
            "is external in the subgraph, and no `@provides` window open where the fetch selects \
             it grants it"
        };
        Err(ComparisonError::new(format!(
            "fetch to {}: `{object_type}.{field_name}` {reason}\n* fetch runs at: {}\n\
             * selected at: {}",
            self.subgraph_name,
            display_path(self.path),
            self.selected_at
                .iter()
                .map(|key| key.as_str())
                .collect::<Vec<_>>()
                .join("."),
        )))
    }
}

/// The states below a field: every runtime type its result can have at each state, each with what
/// the window grants below the field together with the window the field itself opens, pooled. The
/// model's `resolutionFieldStates`.
fn field_states(
    subgraph: &Subgraph<'_>,
    field_name: &Name,
    states: &[ResolutionState],
) -> Result<Vec<ResolutionState>, ComparisonError> {
    let mut below = IndexSet::default();
    for state in states {
        let Some(definition) = subgraph.field(&state.object_type, field_name) else {
            continue;
        };
        let mut window = window_below(
            subgraph.schema(),
            &state.window,
            &state.object_type,
            field_name,
        );
        if let Some(provides) = subgraph.provides(&state.object_type, field_name)? {
            window.extend(provides);
        }
        for object_type in possible_types(subgraph.schema(), definition.ty.inner_named_type()) {
            below.insert(ResolutionState {
                object_type,
                window: window.clone(),
            });
        }
    }
    Ok(below.into_iter().collect())
}

/// One level of a window read at a runtime type: every field it names under type conditions that
/// admit the type, each carrying the window below it, as the model's `windowEntries` reads a level
/// at that type and `WindowGrants` and `windowBelow` keep the entries admitting it.
fn window_entries<'w>(
    schema: &ValidFederationSchema,
    window: &'w [Selection],
    object_type: &Name,
    entries: &mut Vec<&'w Node<executable::Field>>,
) {
    for selection in window {
        match selection {
            Selection::Field(field) => entries.push(field),
            Selection::InlineFragment(fragment) => {
                let admits = admitted_types(schema, fragment.type_condition.as_slice())
                    .is_none_or(|types| types.contains(object_type));
                if admits {
                    window_entries(
                        schema,
                        &fragment.selection_set.selections,
                        object_type,
                        entries,
                    );
                }
            }
            // A field set has no named fragments.
            Selection::FragmentSpread(_) => {}
        }
    }
}

/// `WindowGrants`: a window grants a field on a runtime type when it names the field under
/// conditions admitting that type. A granted leaf has nothing below it, which is a grant still.
fn window_grants(
    schema: &ValidFederationSchema,
    window: &[Selection],
    object_type: &Name,
    field_name: &Name,
) -> bool {
    let mut entries = Vec::new();
    window_entries(schema, window, object_type, &mut entries);
    entries.iter().any(|entry| entry.name == *field_name)
}

/// `windowBelow`: what a window grants below a field on a runtime type, the windows under every
/// occurrence of the field admitting the type, together. Fields are matched by name, a field set
/// carrying no aliases.
fn window_below(
    schema: &ValidFederationSchema,
    window: &[Selection],
    object_type: &Name,
    field_name: &Name,
) -> Vec<Selection> {
    let mut entries = Vec::new();
    window_entries(schema, window, object_type, &mut entries);
    entries
        .iter()
        .filter(|entry| entry.name == *field_name)
        .flat_map(|entry| entry.selection_set.selections.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::Subgraph;
    use super::check_fetch_resolves;
    use crate::correctness::query_plan_check::test_support::recursive_chain;
    use crate::correctness::query_plan_check::test_support::recursive_subgraph;

    /// A chain of 64 `next` through an interface with four implementations completes, which a
    /// descent per runtime path, 4^64 of them, would not: the walk pools the runtime types that
    /// share a window. It shows completion on this input, not a growth law.
    ///
    /// The model's `deepInterfaceChainIsWalkedOnce`.
    #[test]
    fn a_deep_interface_chain_is_walked_once() {
        let subgraph = recursive_subgraph();
        let schema = subgraph.validated_schema();
        let document = recursive_chain(schema, 64);
        let operation = document.operations.get(None).expect("one operation");
        check_fetch_resolves(
            &Subgraph::new(schema).expect("subgraph metadata"),
            "recursive",
            &[],
            &operation.selection_set,
        )
        .expect("the subgraph can resolve every field of the chain");
    }
}
