//! Selection-set helpers the checker shares: inlining a fetch's own fragment spreads, the object
//! types a type condition grounds to, and following key renamers through a selection set.
//!
//! Port of the structural helpers in `Apollo/Definitions/QueryPlanSelections.lean`.
//!
//! # Fragment spreads
//!
//! A fetch's operation is read into the response where it runs, in its own subgraph's terms, and
//! fragment names are per-document, so a fetch's spreads are inlined at that boundary, each
//! becoming an inline fragment carrying the definition's type condition.
//!
//! # Key renamers
//!
//! Input rewrites rename keys in the representation a fetch sends. Soundness reads them in both
//! directions — forwards to check an entry against them, inverted to turn a subgraph's demand into
//! what must already have been fetched — so the walk is shared and parameterized by which positions
//! a `TypenameEquals` filter applies at: [`type_filter_meets`] forwards and [`type_filter_covers`]
//! inverted. A fetch's output rewrites rename what it adds to the response instead, which
//! `Response::rename` does on the response itself.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;

use super::ComparisonError;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
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

/// The intersection of two type conditions. `None` is unrestricted, and so is absorbed.
pub(super) fn intersect_types(
    left: Option<Vec<Name>>,
    right: Option<Vec<Name>>,
) -> Option<Vec<Name>> {
    match (left, right) {
        (None, right) => right,
        (left, None) => left,
        (Some(left), Some(right)) => {
            Some(left.into_iter().filter(|ty| right.contains(ty)).collect())
        }
    }
}

//==================================================================================================
// Key renamers
//==================================================================================================

/// Which positions a `TypenameEquals` filter applies at: the filter's own object types and the
/// fragment guards in force where a key was reached.
type TypeFilterTest = fn(&ValidFederationSchema, &[Name], &[Name]) -> bool;

/// Applies the key renamers among a fetch's rewrites, in order, as `apply_rewrites` does.
///
/// A value setter changes a value but no key, so it is passed over: `apply_output_rewrite` rejects
/// one in an output, and on the input side one fakes the `__typename` an interface object is sent
/// with, which nothing here compares.
pub(super) fn apply_key_renamers(
    schema: &ValidFederationSchema,
    applies: TypeFilterTest,
    rewrites: &[Arc<FetchDataRewrite>],
    selections: Vec<Selection>,
) -> Vec<Selection> {
    let mut selections = selections;
    for rewrite in rewrites {
        let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
            continue;
        };
        selections = rename_key_at(
            schema,
            applies,
            &[],
            &[],
            &renamer.path,
            &renamer.rename_key_to,
            selections,
        );
    }
    selections
}

/// The inverse of [`apply_key_renamers`]: every key renamer inverted, the last first, so that each
/// path is followed through the keys it was written against.
///
/// One renamer inverted names the key it renames *to* back to the key it renames, at the same
/// position. A path that does not end at a key renames nothing, as `split_path_last_element` finds
/// no key there.
pub(super) fn invert_key_renamers(
    schema: &ValidFederationSchema,
    applies: TypeFilterTest,
    rewrites: &[Arc<FetchDataRewrite>],
    selections: Vec<Selection>,
) -> Vec<Selection> {
    let mut selections = selections;
    for rewrite in rewrites.iter().rev() {
        let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
            continue;
        };
        let Some((FetchDataPathElement::Key(renamed_key, type_condition), leading)) =
            renamer.path.split_last()
        else {
            continue;
        };
        let mut path = leading.to_vec();
        path.push(FetchDataPathElement::Key(
            renamer.rename_key_to.clone(),
            type_condition.clone(),
        ));
        selections = rename_key_at(schema, applies, &[], &[], &path, renamed_key, selections);
    }
    selections
}

/// Applies one key renamer, following `rename_at_path`: the path is followed through the fetch's
/// selections and the key it ends at is renamed; a key the path does not reach is left alone.
#[allow(clippy::too_many_arguments)]
fn rename_key_at(
    schema: &ValidFederationSchema,
    applies: TypeFilterTest,
    type_filter: &[Name],
    guards: &[Name],
    path: &[FetchDataPathElement],
    new_key: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    let Some((head, rest)) = path.split_first() else {
        return selections;
    };
    match head {
        FetchDataPathElement::Key(name, _) if rest.is_empty() => rename_here(
            schema,
            applies,
            type_filter,
            guards,
            name,
            new_key,
            selections,
        ),
        FetchDataPathElement::Key(name, _) => {
            rename_key_under(schema, applies, name, rest, new_key, selections)
        }
        FetchDataPathElement::TypenameEquals(type_name) => {
            let mut type_filter = type_filter.to_vec();
            type_filter.push(type_name.clone());
            rename_key_at(
                schema,
                applies,
                &type_filter,
                guards,
                rest,
                new_key,
                selections,
            )
        }
        // An index is passed over: a selection set does not tell a list from its elements, and
        // `apply_rewrites` renames in every element of an array. `rename_at_path` rejects one.
        FetchDataPathElement::AnyIndex(_) => rename_key_at(
            schema,
            applies,
            type_filter,
            guards,
            rest,
            new_key,
            selections,
        ),
        // `Parent` moves up, so it cannot be followed downwards.
        FetchDataPathElement::Parent => selections,
    }
}

/// Descends past one response key, carrying the rest of the path. A new level starts with no type
/// filter and no guards, as `rename_at_path` starts with a fresh one.
fn rename_key_under(
    schema: &ValidFederationSchema,
    applies: TypeFilterTest,
    name: &Name,
    path: &[FetchDataPathElement],
    new_key: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    selections
        .into_iter()
        .map(|selection| match selection {
            Selection::Field(field) if field.response_name() == name => {
                let mut copy = (*field).clone();
                copy.selection_set.selections = rename_key_at(
                    schema,
                    applies,
                    &[],
                    &[],
                    path,
                    new_key,
                    std::mem::take(&mut copy.selection_set.selections),
                );
                Selection::Field(Node::new(copy))
            }
            Selection::InlineFragment(fragment) => {
                let mut copy = (*fragment).clone();
                copy.selection_set.selections = rename_key_under(
                    schema,
                    applies,
                    name,
                    path,
                    new_key,
                    std::mem::take(&mut copy.selection_set.selections),
                );
                Selection::InlineFragment(Node::new(copy))
            }
            selection => selection,
        })
        .collect()
}

/// Renames every selection at one response key, where the path ends. The rename applies only where
/// `applies` admits the `TypenameEquals` filter against the guards in force.
#[allow(clippy::too_many_arguments)]
fn rename_here(
    schema: &ValidFederationSchema,
    applies: TypeFilterTest,
    type_filter: &[Name],
    guards: &[Name],
    name: &Name,
    new_key: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    selections
        .into_iter()
        .map(|selection| match selection {
            Selection::Field(field)
                if field.response_name() == name && applies(schema, type_filter, guards) =>
            {
                let mut copy = (*field).clone();
                copy.alias = Some(new_key.clone());
                Selection::Field(Node::new(copy))
            }
            Selection::InlineFragment(fragment) => {
                let mut guards = guards.to_vec();
                if let Some(type_condition) = &fragment.type_condition {
                    guards.push(type_condition.clone());
                }
                let mut copy = (*fragment).clone();
                copy.selection_set.selections = rename_here(
                    schema,
                    applies,
                    type_filter,
                    &guards,
                    name,
                    new_key,
                    std::mem::take(&mut copy.selection_set.selections),
                );
                Selection::InlineFragment(Node::new(copy))
            }
            selection => selection,
        })
        .collect()
}

/// Does an input rewrite's filter admit *some* object type the position can hold, so that the
/// router may rename there? This is how a renamer is read forwards. A position nothing is known
/// about is met by any filter.
pub(super) fn type_filter_meets(
    schema: &ValidFederationSchema,
    type_filter: &[Name],
    guards: &[Name],
) -> bool {
    let Some(filter_types) = admitted_types(schema, type_filter) else {
        return true;
    };
    let Some(guard_types) = admitted_types(schema, guards) else {
        return true;
    };
    guard_types
        .iter()
        .any(|type_name| filter_types.contains(type_name))
}

/// Does an input rewrite's filter admit *every* object type the position can hold, so that the
/// router renames there whatever the runtime type? This is how a renamer is read inverted, where
/// only a rename that certainly happens may be undone. A position nothing is known about is
/// covered only by a filterless rewrite.
pub(super) fn type_filter_covers(
    schema: &ValidFederationSchema,
    type_filter: &[Name],
    guards: &[Name],
) -> bool {
    let Some(filter_types) = admitted_types(schema, type_filter) else {
        return true;
    };
    let Some(guard_types) = admitted_types(schema, guards) else {
        return false;
    };
    guard_types
        .iter()
        .all(|type_name| filter_types.contains(type_name))
}
