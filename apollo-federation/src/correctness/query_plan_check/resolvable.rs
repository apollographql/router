//! Whether a fetch selects only what its subgraph resolves.
//!
//! Port of `fetchesSelectResolvableBool` in `Apollo/Implementations/QueryPlanChecker.lean`, deciding
//! `FetchesSelectResolvable`. A subgraph resolves every field it declares except one it marks
//! `@external`, which it resolves only where a `@provides` window open at that position grants it.
//! The model's `docs/federation.md` says what the two directives mean and how a window opens.
//!
//! The judgement is per runtime type. A field spelled on an interface is a field of every member
//! the position can hold, one member can mark it `@external` where another does not, and a window
//! conditioned on one member grants nothing to the others. So the walk carries runtime types, each
//! with the window open on it, and reads an inline fragment only at the types it applies to. Types
//! reached with the same window are pooled into one descent; a type reached with two windows is two
//! states, so no type is credited with another's window.
//!
//! The walk reads the fetch's operation from its root in the subgraph's own schema, `_entities`
//! included, so a window opened above an entity fetch's position is not open in it: the subgraph
//! reaches the entity through `_entities`, not through the field that declared the window.
//!
//! `ExternalMetadata::provided_fields` is no substitute for the windows, being one set for the whole
//! subgraph; a window is followed down the fetch's operation as the walk descends.

use apollo_compiler::Name;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::ExtendedType;

use super::ComparisonError;
use super::selections::possible_types;
use super::subgraph::Subgraph;
use crate::schema::ValidFederationSchema;

/// One selection of a `@provides` window: a field-set selection with its arguments left out, since
/// they do not bear on whether a field can be resolved. Without them windows can be compared, so
/// the walk can pool the runtime types it reaches with the same window.
#[derive(Clone, PartialEq, Eq)]
enum WindowSelection {
    Field(Name, Window),
    InlineFragment(Option<Name>, Window),
}

/// What a `@provides` field set grants: field names under type conditions, with the window below
/// each. Two windows open at one position grant what either does, so they are appended.
type Window = Vec<WindowSelection>;

/// A runtime type with the window open on it.
type State = (Name, Window);

/// The window a parsed `@provides` field set opens.
fn window_of(selections: &[Selection]) -> Window {
    selections
        .iter()
        .filter_map(|selection| match selection {
            Selection::Field(field) => Some(WindowSelection::Field(
                field.name.clone(),
                window_of(&field.selection_set.selections),
            )),
            Selection::InlineFragment(fragment) => Some(WindowSelection::InlineFragment(
                fragment.type_condition.clone(),
                window_of(&fragment.selection_set.selections),
            )),
            // A field set has no fragment spreads.
            Selection::FragmentSpread(_) => None,
        })
        .collect()
}

/// Spec `DoesFragmentTypeApply` at a known runtime object type.
fn does_fragment_type_apply(
    schema: &ValidFederationSchema,
    object_type: &Name,
    type_condition: Option<&Name>,
) -> bool {
    type_condition.is_none_or(|condition| possible_types(schema, condition).contains(object_type))
}

/// One level of a window at a runtime type: each field it names there, with the window below that
/// occurrence.
fn fields_at<'w>(
    schema: &ValidFederationSchema,
    object_type: &Name,
    window: &'w Window,
    out: &mut Vec<(&'w Name, &'w Window)>,
) {
    for selection in window {
        match selection {
            WindowSelection::Field(name, below) => out.push((name, below)),
            WindowSelection::InlineFragment(type_condition, selections) => {
                if does_fragment_type_apply(schema, object_type, type_condition.as_ref()) {
                    fields_at(schema, object_type, selections, out);
                }
            }
        }
    }
}

/// A window's grant of a field at a runtime type: `None` if the window does not name the field
/// there, and otherwise the windows below the field's occurrences there, together.
fn grant(
    schema: &ValidFederationSchema,
    object_type: &Name,
    field_name: &Name,
    window: &Window,
) -> Option<Window> {
    let mut fields = Vec::new();
    fields_at(schema, object_type, window, &mut fields);
    let mut occurrences = fields
        .into_iter()
        .filter(|(name, _)| *name == field_name)
        .peekable();
    occurrences.peek()?;
    Some(
        occurrences
            .flat_map(|(_, below)| below.iter().cloned())
            .collect(),
    )
}

/// The named type a field of an object type returns, in the subgraph's schema.
fn field_type(
    schema: &ValidFederationSchema,
    object_type: &Name,
    field_name: &Name,
) -> Option<Name> {
    match schema.schema().types.get(object_type)? {
        ExtendedType::Object(ty) => Some(ty.fields.get(field_name)?.ty.inner_named_type().clone()),
        _ => None,
    }
}

struct Walk<'a> {
    subgraph: Subgraph<'a>,
    schema: &'a ValidFederationSchema,
}

impl Walk<'_> {
    /// The window open below a field selected at a runtime type: what the window open at the field
    /// grants below it, and what the field's own `@provides` opens.
    fn window_below(
        &self,
        object_type: &Name,
        field_name: &Name,
        window: &Window,
    ) -> Result<Window, ComparisonError> {
        let mut below = grant(self.schema, object_type, field_name, window).unwrap_or_default();
        if let Some(fields) = self.subgraph.provides(object_type, field_name)?
            && let Some(child_type) = field_type(self.schema, object_type, field_name)
        {
            let parsed = FieldSet::parse_and_validate(
                self.schema.schema(),
                child_type,
                &fields,
                "provides.graphql",
            )
            .map_err(|e| {
                ComparisonError::new(format!(
                    "@provides on {object_type}.{field_name} is not a valid field set:\n{e}"
                ))
            })?;
            below.extend(window_of(&parsed.selection_set.selections));
        }
        Ok(below)
    }

    /// Whether a selection set, read at every state of a pool, selects only what the subgraph
    /// resolves: `None` if it does, or why the first field it does not is unresolvable. `Err` is a
    /// malformed `@provides`.
    fn selections_resolvable(
        &self,
        states: &[State],
        selections: &[Selection],
    ) -> Result<Option<String>, ComparisonError> {
        for selection in selections {
            let unresolvable = match selection {
                Selection::Field(field) => {
                    // Introspection is the router's and the subgraph's alike, never external.
                    if !field.name.starts_with("__") {
                        for (object_type, window) in states {
                            if self.subgraph.is_external(object_type, &field.name)
                                && grant(self.schema, object_type, &field.name, window).is_none()
                            {
                                return Ok(Some(format!(
                                    "`{object_type}.{}` is `@external` there, \
                                     and no `@provides` window open at it grants it",
                                    field.name
                                )));
                            }
                        }
                    }
                    let mut below: Vec<State> = Vec::new();
                    for (object_type, window) in states {
                        let Some(child_type) = field_type(self.schema, object_type, &field.name)
                        else {
                            continue;
                        };
                        let child_window = self.window_below(object_type, &field.name, window)?;
                        for runtime_type in possible_types(self.schema, &child_type) {
                            let state = (runtime_type, child_window.clone());
                            if !below.contains(&state) {
                                below.push(state);
                            }
                        }
                    }
                    self.selections_resolvable(&below, &field.selection_set.selections)?
                }
                Selection::InlineFragment(fragment) => {
                    let applying: Vec<State> = states
                        .iter()
                        .filter(|(object_type, _)| {
                            does_fragment_type_apply(
                                self.schema,
                                object_type,
                                fragment.type_condition.as_ref(),
                            )
                        })
                        .cloned()
                        .collect();
                    self.selections_resolvable(&applying, &fragment.selection_set.selections)?
                }
                // Spreads are inlined before a fetch's operation is read.
                Selection::FragmentSpread(_) => None,
            };
            if unresolvable.is_some() {
                return Ok(unresolvable);
            }
        }
        Ok(None)
    }
}

/// Whether a fetch's operation, its spreads inlined, selects only what its subgraph resolves, read
/// from `root_type` in the subgraph's schema with no window open there.
pub(super) fn check_fetch_selects_resolvable(
    subgraph_name: &str,
    subgraph_schema: &ValidFederationSchema,
    root_type: &Name,
    selections: &[Selection],
) -> Result<(), ComparisonError> {
    let walk = Walk {
        subgraph: Subgraph::new(subgraph_schema)?,
        schema: subgraph_schema,
    };
    match walk.selections_resolvable(&[(root_type.clone(), Window::new())], selections)? {
        None => Ok(()),
        Some(reason) => Err(ComparisonError::new(format!(
            "fetch to {subgraph_name} selects what the subgraph does not resolve: {reason}"
        ))),
    }
}
