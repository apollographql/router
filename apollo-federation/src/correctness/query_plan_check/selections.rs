//! Selection-set helpers the checker shares: inlining a fetch's own fragment spreads and the object
//! types a type condition grounds to.
//!
//! Port of the structural helpers in `Apollo/Definitions/QueryPlanSelections.lean`.
//!
//! # Fragment spreads
//!
//! A fetch's operation is read into the response where it runs, in its own subgraph's terms, and
//! fragment names are per-document, so a fetch's spreads are inlined at that boundary, each
//! becoming an inline fragment carrying the definition's type condition.
//!
//! A fetch's input rewrites are followed over the representation it sends (`representation.rs`),
//! and its output rewrites rename what it adds to the response, which `Response::rename` does on
//! the response itself.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;

use super::ComparisonError;
use crate::schema::ValidFederationSchema;

/// A fetch's selections with its own fragment spreads inlined, each becoming an inline fragment
/// carrying the definition's type condition, the spread's directives and the definition's
/// selections.
///
/// Terminates because a valid document has no fragment reference cycles.
pub(super) fn inline_fragment_spreads(
    selections: &[Selection],
    fragments: &FragmentMap,
) -> Result<Vec<Selection>, ComparisonError> {
    let mut out = Vec::with_capacity(selections.len());
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                let mut copy = (**field).clone();
                copy.selection_set.selections =
                    inline_fragment_spreads(&field.selection_set.selections, fragments)?;
                out.push(Selection::Field(Node::new(copy)));
            }
            Selection::InlineFragment(fragment) => {
                let mut copy = (**fragment).clone();
                copy.selection_set.selections =
                    inline_fragment_spreads(&fragment.selection_set.selections, fragments)?;
                out.push(Selection::InlineFragment(Node::new(copy)));
            }
            Selection::FragmentSpread(spread) => {
                let definition = fragments.get(&spread.fragment_name).ok_or_else(|| {
                    ComparisonError::new(format!(
                        "fetch operation spreads `{}`, which it does not define",
                        spread.fragment_name
                    ))
                })?;
                out.push(Selection::InlineFragment(Node::new(InlineFragment {
                    type_condition: Some(definition.type_condition().clone()),
                    directives: spread.directives.clone(),
                    selection_set: selection_set(
                        definition.selection_set.ty.clone(),
                        inline_fragment_spreads(&definition.selection_set.selections, fragments)?,
                    ),
                })))
            }
        }
    }
    Ok(out)
}

/// Wraps a selection set in one selection unless it is empty, so a path that matches nothing
/// contributes nothing.
pub(super) fn wrap_non_empty(
    wrap: impl FnOnce(Vec<Selection>) -> Selection,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    if selections.is_empty() {
        Vec::new()
    } else {
        vec![wrap(selections)]
    }
}

/// Guards a selection set by a list of directives, on one inline fragment with no type condition.
///
/// No directives means no guard, and an empty selection set stays empty rather than becoming an
/// empty guarded fragment.
pub(super) fn under_directives(
    directives: &ast::DirectiveList,
    parent_type: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    if directives.is_empty() {
        return selections;
    }
    let directives = directives.clone();
    let parent_type = parent_type.clone();
    wrap_non_empty(
        move |selections| {
            Selection::InlineFragment(Node::new(InlineFragment {
                type_condition: None,
                directives,
                selection_set: selection_set(parent_type, selections),
            }))
        },
        selections,
    )
}

fn selection_set(ty: Name, selections: Vec<Selection>) -> SelectionSet {
    let mut set = SelectionSet::new(ty);
    set.selections = selections;
    set
}

/// The object types a type name grounds to, sorted. Empty if the schema does not declare it, or it
/// is not composite.
pub(super) fn possible_types(schema: &ValidFederationSchema, type_name: &Name) -> Vec<Name> {
    let Ok(position) = schema.get_type(type_name) else {
        return Vec::new();
    };
    let Ok(composite) = position.try_into() else {
        return Vec::new();
    };
    let Ok(types) = schema.possible_runtime_types(composite) else {
        return Vec::new();
    };
    let mut names: Vec<Name> = types.into_iter().map(|ty| ty.type_name).collect();
    names.sort();
    names
}

/// The type condition a list of written conditions jointly imposes: the intersection of the object
/// types each admits. `None` — rather than an empty list — means unrestricted.
pub(super) fn admitted_types(schema: &ValidFederationSchema, guards: &[Name]) -> Option<Vec<Name>> {
    let mut admitted: Option<Vec<Name>> = None;
    for guard in guards {
        let types = possible_types(schema, guard);
        admitted = Some(match admitted {
            None => types,
            Some(current) => types
                .into_iter()
                .filter(|ty| current.contains(ty))
                .collect(),
        });
    }
    admitted
}
