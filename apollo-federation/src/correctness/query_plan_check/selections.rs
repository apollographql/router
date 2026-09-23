//! Building the selection set a plan fetches.
//!
//! Port of the structural helpers in `Apollo/Definitions/QueryPlanCompleteness.lean`. A query plan
//! says *where* each fetch runs — a flatten path of response keys — and *what* it selects there.
//! Turning that back into one operation means mounting each fetch's selections at its path, which
//! is what [`selections_at`] does.
//!
//! A fetch data path names **response keys, not fields**, so the field a path element stands for
//! can only be recovered by following the path through what has already been fetched. A key may
//! also be reached by more than one field, so following it reproduces *every* copy. Two elements
//! reach nothing and appear in no flatten path: `TypenameEquals` addresses data by runtime type
//! rather than by response key, and `Parent` moves up, so it cannot be followed downwards. Both
//! are used only by fetch data rewrites — and by `@context`, which this checker does not verify,
//! as the legacy one does not either.
//!
//! # Fragment spreads
//!
//! `query_compare` resolves spreads where it reaches them, symmetrically with inline fragments,
//! and the client operation is compared that way. A fetch's selections cannot be: everything a
//! plan fetches accumulates into **one** buffer whose selections come from as many documents as
//! there are fetches, fragment names are per-document, and two subgraph operations may each define
//! `F` differently. There is no single fragment map under which the merged buffer resolves, so the
//! spreads have to go at the boundary, as the legacy checker effectively does when it converts
//! each fetch operation to a response shape.
//!
//! # Output rewrites
//!
//! What a fetch contributes is what it selects, under the key renames its output rewrites apply.
//! A rename is what makes a `@requires` field reachable under the key the fetch's `requires` entry
//! names, when the planner had to alias it — two entity types requiring the same field at
//! different types cannot both select it unaliased. Input rewrites need no counterpart:
//! `check_input_rewrite` accepts only a value setter and ignores it, since the one the planner
//! emits overwrites `__typename` and so leaves the response shape alone.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;

use super::super::query_compare::conditions::BooleanLiteral;
use super::ComparisonError;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Accumulating fetch selection set
//==================================================================================================

/// The possible object type names a path element is restricted to, read as a disjunction.
/// `None` — rather than an empty list — means unrestricted.
///
/// The query plan calls these `Conditions`, which invites confusion with the Boolean
/// `@skip`/`@include` conditions on a condition node. These are *type* conditions.
pub(super) type TypeCondition = [Name];

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

/// Guards a selection set by the condition branches it sits under, one nested inline fragment per
/// branch.
///
/// No condition means no guard, and an empty selection set stays empty rather than becoming an
/// empty guarded fragment.
pub(super) fn under_condition(
    condition: &[BooleanLiteral],
    parent_type: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    // One fragment per literal, nested: conjoining them would repeat `@include` or `@skip`,
    // which are not repeatable.
    condition
        .iter()
        .rev()
        .fold(selections, |selections, literal| {
            under_directives(
                &ast::DirectiveList(vec![literal.to_directive().into()]),
                parent_type,
                selections,
            )
        })
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

/// Does `pending` admit a fragment carrying `fragment_type`?
///
/// An unrestricted pending condition admits everything, and so does a fragment with no type
/// condition of its own.
fn type_condition_admits(
    schema: &ValidFederationSchema,
    pending: Option<&TypeCondition>,
    fragment_type: Option<&Name>,
) -> bool {
    let (Some(type_names), Some(fragment_type)) = (pending, fragment_type) else {
        return true;
    };
    // The path's condition names the concrete types the data there is narrowed to; the fragment's
    // own condition may be abstract. So the question is whether the fragment reaches those types,
    // not whether they reach it -- asking it the other way admits only a fragment written on the
    // exact same object type, and skips one written on any interface or union above it.
    type_names
        .iter()
        .any(|type_name| type_includes_object(schema, fragment_type, type_name))
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

/// Is `object_name` one of the runtime types of `type_name`?
fn type_includes_object(
    schema: &ValidFederationSchema,
    type_name: &Name,
    object_name: &Name,
) -> bool {
    let Ok(position) = schema.get_type(type_name) else {
        return false;
    };
    let Ok(composite) = position.try_into() else {
        return false;
    };
    schema
        .possible_runtime_types(composite)
        .map(|types| types.iter().any(|ty| ty.type_name == *object_name))
        .unwrap_or(false)
}

/// The selections that `mounted` becomes when placed at `path` inside `available`.
///
/// `pending` is the type condition carried from the previous path element. It filters *where* the
/// next key may be found rather than adding a guard of its own.
pub(super) fn selections_at(
    schema: &ValidFederationSchema,
    available: &[Selection],
    pending: Option<&TypeCondition>,
    path: &[FetchDataPathElement],
    mounted: &[Selection],
) -> Vec<Selection> {
    let Some((head, rest)) = path.split_first() else {
        return mounted.to_vec();
    };
    match head {
        FetchDataPathElement::Key(name, type_condition) => selections_under_key(
            schema,
            available,
            pending,
            name,
            type_condition.as_deref(),
            rest,
            mounted,
        ),
        FetchDataPathElement::AnyIndex(type_condition) => {
            selections_at(schema, available, type_condition.as_deref(), rest, mounted)
        }
        FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => Vec::new(),
    }
}

/// Every copy of a response key reachable in `available`, carrying the rest of the path.
///
/// Inline fragments admitted by `pending` are descended into and reproduced, so a reached field
/// keeps the type condition that guarded it.
#[allow(clippy::too_many_arguments)]
fn selections_under_key(
    schema: &ValidFederationSchema,
    available: &[Selection],
    pending: Option<&TypeCondition>,
    name: &Name,
    type_condition: Option<&TypeCondition>,
    path: &[FetchDataPathElement],
    mounted: &[Selection],
) -> Vec<Selection> {
    let mut out = Vec::new();
    for selection in available {
        match selection {
            Selection::Field(field) => {
                if field.response_key() != name {
                    continue;
                }
                let inner = selections_at(
                    schema,
                    &field.selection_set.selections,
                    type_condition,
                    path,
                    mounted,
                );
                out.extend(wrap_non_empty(
                    |selections| {
                        let mut copy = (**field).clone();
                        copy.selection_set =
                            selection_set(field.selection_set.ty.clone(), selections);
                        Selection::Field(Node::new(copy))
                    },
                    inner,
                ));
            }
            Selection::InlineFragment(fragment) => {
                if !type_condition_admits(schema, pending, fragment.type_condition.as_ref()) {
                    continue;
                }
                let inner = selections_under_key(
                    schema,
                    &fragment.selection_set.selections,
                    pending,
                    name,
                    type_condition,
                    path,
                    mounted,
                );
                out.extend(wrap_non_empty(
                    |selections| {
                        let mut copy = (**fragment).clone();
                        copy.selection_set =
                            selection_set(fragment.selection_set.ty.clone(), selections);
                        Selection::InlineFragment(Node::new(copy))
                    },
                    inner,
                ));
            }
            // Unreachable: `available` holds only what fetches contributed, and
            // `inline_fragment_spreads` removed their spreads at the mount boundary.
            Selection::FragmentSpread(_) => {}
        }
    }
    out
}

//==================================================================================================
// Output rewrites
//==================================================================================================

/// What a fetch contributes, under the key renames its output rewrites apply.
///
/// Only a key renamer can appear here — `apply_output_rewrite` rejects a value setter in an
/// output.
pub(super) fn apply_output_rewrites(
    schema: &ValidFederationSchema,
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
            &[],
            &[],
            &renamer.path,
            &renamer.rename_key_to,
            selections,
        );
    }
    selections
}

/// Applies one key renamer, following `rename_at_path`: the path is followed through the fetch's
/// selections and the key it ends at is renamed; a key the path does not reach is left alone.
fn rename_key_at(
    schema: &ValidFederationSchema,
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
        FetchDataPathElement::Key(name, _) if rest.is_empty() => {
            rename_here(schema, type_filter, guards, name, new_key, selections)
        }
        FetchDataPathElement::Key(name, _) => {
            rename_key_under(schema, name, rest, new_key, selections)
        }
        FetchDataPathElement::TypenameEquals(type_name) => {
            let mut type_filter = type_filter.to_vec();
            type_filter.push(type_name.clone());
            rename_key_at(schema, &type_filter, guards, rest, new_key, selections)
        }
        // An index consumes no response key, and `Parent` cannot be followed downwards.
        FetchDataPathElement::AnyIndex(_) | FetchDataPathElement::Parent => selections,
    }
}

/// Descends past one response key, carrying the rest of the path. A new level starts with no type
/// filter and no guards, as `rename_at_path` starts with a fresh one.
fn rename_key_under(
    schema: &ValidFederationSchema,
    name: &Name,
    path: &[FetchDataPathElement],
    new_key: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    selections
        .into_iter()
        .map(|selection| match selection {
            Selection::Field(field) if field.response_key() == name => {
                let mut copy = (*field).clone();
                copy.selection_set.selections = rename_key_at(
                    schema,
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
/// the `TypenameEquals` filter admits the guards in force.
fn rename_here(
    schema: &ValidFederationSchema,
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
                if field.response_key() == name
                    && type_filter_admits(schema, type_filter, guards) =>
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

/// Does a `TypenameEquals` filter apply to data guarded by `guards` — are the filter's object
/// types all admitted there? An unguarded position is not narrower than the filter, so the filter
/// applies.
fn type_filter_admits(
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
    filter_types
        .iter()
        .all(|type_name| guard_types.contains(type_name))
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::schema::Schema;

    use super::*;

    const SCHEMA: &str = r#"
        type Query { feed: [Item!]! }
        interface Item { id: ID! }
        interface Media implements Item { id: ID! }
        type Book implements Item { id: ID! }
        type Film implements Item & Media { id: ID!, minutes: Int! }
        union Printed = Book
    "#;

    fn schema() -> ValidFederationSchema {
        let schema = Schema::parse_and_validate(SCHEMA, "schema.graphql").unwrap();
        ValidFederationSchema::new(schema).unwrap()
    }

    /// A path element's type condition names the concrete types the data there is narrowed to;
    /// the fragment guarding a fetched selection may be written on anything above them. The two
    /// meet when the fragment reaches those types -- asking it the other way round admits only a
    /// fragment on the exact same object type, which is what once made the checker report
    /// type-conditioned plans as fetching nothing.
    #[test]
    fn a_path_condition_is_admitted_by_a_fragment_above_it() {
        let schema = schema();
        let at_film = vec![name!("Film")];
        for guard in [name!("Film"), name!("Media"), name!("Item")] {
            assert!(
                type_condition_admits(&schema, Some(&at_film), Some(&guard)),
                "`... on {guard}` should reach data narrowed to Film"
            );
        }
    }

    #[test]
    fn a_path_condition_is_not_admitted_by_a_fragment_beside_it() {
        let schema = schema();
        let at_film = vec![name!("Film")];
        for guard in [name!("Book"), name!("Printed")] {
            assert!(
                !type_condition_admits(&schema, Some(&at_film), Some(&guard)),
                "`... on {guard}` should not reach data narrowed to Film"
            );
        }
    }

    /// Several narrowed types meet a fragment that reaches any one of them.
    #[test]
    fn any_of_several_path_types_is_enough() {
        let schema = schema();
        let at_either = vec![name!("Book"), name!("Film")];
        assert!(type_condition_admits(
            &schema,
            Some(&at_either),
            Some(&name!("Media"))
        ));
        assert!(type_condition_admits(
            &schema,
            Some(&at_either),
            Some(&name!("Printed"))
        ));
    }

    /// An unconditioned path element, or an unguarded selection, constrains nothing. This is the
    /// case every plan takes without `--type-conditioned-fetching`, which is why the swapped
    /// comparison above stayed invisible for so long.
    #[test]
    fn an_absent_condition_admits_everything() {
        let schema = schema();
        assert!(type_condition_admits(&schema, None, Some(&name!("Book"))));
        assert!(type_condition_admits(&schema, Some(&[name!("Film")]), None));
        assert!(type_condition_admits(&schema, None, None));
    }
}
