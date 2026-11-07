//! Building the selection set a plan fetches.
//!
//! Port of the structural helpers in `Apollo/Definitions/QueryPlanCompleteness.lean`. A query plan
//! says *where* each fetch runs — a flatten path of response keys — and *what* it selects there.
//! Turning that back into one operation means mounting each fetch's selections at its path, which
//! is what [`selections_at`] does.
//!
//! A fetch data path names **response keys, not fields**, so the field a path element stands for
//! can only be recovered by following the path through what has already been fetched. A key may
//! also be reached by more than one field, so following it reproduces *every* distinct copy. Two
//! elements reach nothing and appear in no flatten path: `TypenameEquals` addresses data by
//! runtime type rather than by response key, and `Parent` moves up, so it cannot be followed
//! downwards. Both are used only by fetch data rewrites, `@context`'s among them, whose paths
//! `context.rs` follows itself: a leading `Parent` resolved against the path the fetch runs at,
//! a `TypenameEquals` read as narrowing the level it stands at.
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
//! # Key renamers
//!
//! What a fetch contributes is what it selects, under the key renames its output rewrites apply.
//! A rename is what makes a `@requires` field reachable under the key the fetch's `requires` entry
//! names, when the planner had to alias it — two entity types requiring the same field at
//! different types cannot both select it unaliased.
//!
//! Input rewrites rename the other way: the representation a fetch sends. Soundness reads them in
//! both directions — forwards to check an entry against them, inverted to turn a subgraph's demand
//! into what must already have been fetched — so the walk is shared and parameterized by which
//! positions a `TypenameEquals` filter applies at: [`type_filter_admits`] on an output,
//! [`type_filter_meets`] forwards and [`type_filter_covers`] inverted on an input.

use std::borrow::Cow;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::conditions::BooleanLiteral;
use super::ComparisonError;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Accumulating fetch selection set
//==================================================================================================

/// The possible object type names a path element is restricted to, read as a disjunction and
/// compared literally with each object's `__typename`, as the router's `iterate_path` compares
/// them, so a name that is no object type matches nothing. `None` — rather than an empty list —
/// means unrestricted.
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

/// Guards selections by the runtime types a path element narrowed the position to, so that a
/// requirement is mounted only where the fetch runs.
/// - A selection written under a fragment already carries its own condition; this is for the
///   unguarded case, where the narrowing would otherwise be lost.
/// - The condition's names are object names, compared literally with each object's `__typename`
///   as [`type_condition_admits`] explains, so only a name that is an object type takes a copy,
///   each its own, a disjunction: an abstract name selects no object, and a condition naming no
///   object type contributes nothing. An object type the position cannot hold keeps a copy no
///   object there satisfies, which inclusion credits with nothing.
/// - An unnarrowed position is returned untouched, which is every position in a plan built
///   without `--type-conditioned-fetching`.
fn under_reached_types(
    schema: &ValidFederationSchema,
    reached: Option<&TypeCondition>,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    let Some(type_names) = reached else {
        return selections;
    };
    if selections.is_empty() {
        return selections;
    }
    type_names
        .iter()
        .filter(|type_name| {
            matches!(
                schema.schema().types.get(*type_name),
                Some(ExtendedType::Object(_))
            )
        })
        .map(|type_name| {
            Selection::InlineFragment(Node::new(InlineFragment {
                type_condition: Some(type_name.clone()),
                directives: Default::default(),
                selection_set: selection_set(type_name.clone(), selections.clone()),
            }))
        })
        .collect()
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

/// Does `pending` admit a fragment whose runtime types are `reached` — is one of its names an
/// object type the fragment reaches?
///
/// The condition's names are compared literally with the `__typename` of each object at its
/// level, as the router's `iterate_path` compares them, so a name that is no object type, an
/// interface's or a union's, admits nothing, and `[I, A]` admits what `[A]` does. An unrestricted
/// pending condition admits everything, and so does a fragment with no type condition of its own,
/// whose `reached` is `None`.
fn type_condition_admits(pending: Option<&TypeCondition>, reached: Option<&[Name]>) -> bool {
    let (Some(type_names), Some(reached)) = (pending, reached) else {
        return true;
    };
    // The fragment's own condition may be abstract. So the question is whether the fragment
    // reaches the path's types, not whether they reach it -- asking it the other way admits only a
    // fragment written on the exact same object type, and skips one written on any interface or
    // union above it.
    type_names
        .iter()
        .any(|type_name| reached.contains(type_name))
}

/// What a path element's narrowing comes to inside an inline fragment it admits, the fragment
/// reaching `reached`: what the path and the fragment leave between them, the narrowed types the
/// fragment reaches.
///
/// A fragment whose runtime types are all among those types states the narrowing itself, so
/// nothing is left for a field below it to be guarded by, and the field is reproduced under the
/// fragments alone, one copy admitting every runtime type the fragment reaches. Guarding it again
/// would add a copy per type, each narrowed to that type alone: under a fragment on an object type
/// all but one contradict it, and under a fragment on an abstract type they are compatible copies
/// narrowed apart, which the context reader, needing one copy that admits every type past the
/// levels it shares with a fetch, rejects. A fragment with no type condition narrows nothing.
fn pending_inside<'a>(
    pending: Option<&'a TypeCondition>,
    reached: Option<&[Name]>,
) -> Option<Cow<'a, TypeCondition>> {
    let type_names = pending?;
    let Some(reached) = reached else {
        return Some(Cow::Borrowed(type_names));
    };
    if reached
        .iter()
        .all(|type_name| type_names.contains(type_name))
    {
        return None;
    }
    Some(Cow::Owned(
        type_names
            .iter()
            .filter(|type_name| reached.contains(type_name))
            .cloned()
            .collect(),
    ))
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

/// The selections that `mounted` becomes when placed at `path` inside `available`.
///
/// `pending` is the type condition carried from the previous path element. It filters *where* the
/// next key may be found, or what is mounted where the path ends ([`under_type_condition`]).
pub(super) fn selections_at(
    schema: &ValidFederationSchema,
    available: &[Selection],
    pending: Option<&TypeCondition>,
    path: &[FetchDataPathElement],
    mounted: &[Selection],
) -> Vec<Selection> {
    let Some((head, rest)) = path.split_first() else {
        return under_type_condition(schema, pending, mounted);
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
        // An index consumes no response key: its condition and the one carried from the key
        // before it are about the same objects, so both hold.
        FetchDataPathElement::AnyIndex(type_condition) => {
            let conjoined = intersect_types(pending.map(<[Name]>::to_vec), type_condition.clone());
            selections_at(schema, available, conjoined.as_deref(), rest, mounted)
        }
        FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => Vec::new(),
    }
}

/// Every distinct copy of a response key reachable in `available`, carrying the rest of the path.
///
/// Inline fragments admitted by `pending` are descended into and reproduced, so a reached field
/// keeps the type condition that guarded it. Inside a fragment what is carried is the narrowing
/// the path and the fragment leave between them ([`pending_inside`]): a field found below
/// fragments that state the narrowing is reproduced under them alone, with no guard of its own, so
/// a condition of `n` types over `n` sibling fragments mounts one copy per fragment.
///
/// A reproduced copy that repeats one already reproduced here exactly, source locations aside, is
/// kept once, where it first occurs. Every fetch mounted at or below a key leaves its own copy of
/// the path to it, and a later fetch mounted there reproduces each of them as the same selection:
/// the path as it was first selected, holding what that one fetch mounts. The buffer is read as a
/// response shape, in which equal selections merge, so a repetition adds nothing but its size;
/// kept, the copies would double with every fetch along a chain of fetches at one site. Copies
/// that differ in anything else, a guard included, are all kept, in their order.
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
    let mut out = IndexSet::default();
    for selection in available {
        match selection {
            Selection::Field(field) => {
                if field.response_name() != name {
                    continue;
                }
                let inner = selections_at(
                    schema,
                    &field.selection_set.selections,
                    type_condition,
                    path,
                    mounted,
                );
                let reproduced = wrap_non_empty(
                    |selections| {
                        let mut copy = (**field).clone();
                        copy.selection_set =
                            selection_set(field.selection_set.ty.clone(), selections);
                        Selection::Field(Node::new(copy))
                    },
                    inner,
                );
                // An unguarded field is selected on every type the path narrowed away, unless the
                // fragments it was found under state the narrowing and leave no `pending`.
                out.extend(under_reached_types(schema, pending, reproduced));
            }
            Selection::InlineFragment(fragment) => {
                // The fragment's runtime types, read once for whether it is admitted and for what
                // it leaves of the narrowing, and only where the position is narrowed.
                let reached = pending
                    .and(fragment.type_condition.as_ref())
                    .map(|fragment_type| possible_types(schema, fragment_type));
                if !type_condition_admits(pending, reached.as_deref()) {
                    continue;
                }
                let inside = pending_inside(pending, reached.as_deref());
                let inner = selections_under_key(
                    schema,
                    &fragment.selection_set.selections,
                    inside.as_deref(),
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
    out.into_iter().collect()
}

/// Guards what is mounted where a path ends by the type condition of its last element, as
/// [`selections_under_key`] guards what it finds below one: a fragment the condition admits is
/// kept with what is inside it guarded by what the two leave between them ([`pending_inside`]), one
/// it does not admit is dropped, and an unguarded field takes [`under_reached_types`]'s copies.
///
/// The router selects the objects at the end of a path by the same literal comparison as along it,
/// so a fetch below a condition naming no object of its position never runs, and what it would
/// have contributed, or been owed, is nothing. An unnarrowed end mounts what it is given, and so
/// does one whose fragments state the condition, which is how an entity fetch's selections arrive.
fn under_type_condition(
    schema: &ValidFederationSchema,
    pending: Option<&TypeCondition>,
    selections: &[Selection],
) -> Vec<Selection> {
    let Some(type_names) = pending else {
        return selections.to_vec();
    };
    let mut out = Vec::new();
    for selection in selections {
        match selection {
            Selection::Field(_) => out.extend(under_reached_types(
                schema,
                Some(type_names),
                vec![selection.clone()],
            )),
            Selection::InlineFragment(fragment) => {
                let reached = fragment
                    .type_condition
                    .as_ref()
                    .map(|fragment_type| possible_types(schema, fragment_type));
                if !type_condition_admits(Some(type_names), reached.as_deref()) {
                    continue;
                }
                let inside = pending_inside(Some(type_names), reached.as_deref());
                let inner = under_type_condition(
                    schema,
                    inside.as_deref(),
                    &fragment.selection_set.selections,
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
            // Unreachable: what is mounted is a fetch's contribution, whose spreads
            // `inline_fragment_spreads` removed at the mount boundary.
            Selection::FragmentSpread(_) => {}
        }
    }
    out
}

//==================================================================================================
// Output rewrites
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

/// Does an output rewrite's `TypenameEquals` filter apply to data guarded by `guards` — are the
/// filter's object types all admitted there? An unguarded position is not narrower than the
/// filter, so the filter applies.
pub(super) fn type_filter_admits(
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

    use super::super::test_support::selections;
    use super::super::test_support::under;
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
                type_condition_admits(Some(&at_film), Some(&possible_types(&schema, &guard))),
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
                !type_condition_admits(Some(&at_film), Some(&possible_types(&schema, &guard))),
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
            Some(&at_either),
            Some(&possible_types(&schema, &name!("Media")))
        ));
        assert!(type_condition_admits(
            Some(&at_either),
            Some(&possible_types(&schema, &name!("Printed")))
        ));
    }

    /// An unconditioned path element, or an unguarded selection, constrains nothing. This is the
    /// case every plan takes without `--type-conditioned-fetching`, which is why the swapped
    /// comparison above stayed invisible for so long.
    #[test]
    fn an_absent_condition_admits_everything() {
        let schema = schema();
        let book = possible_types(&schema, &name!("Book"));
        assert!(type_condition_admits(None, Some(&book)));
        assert!(type_condition_admits(Some(&[name!("Film")]), None));
        assert!(type_condition_admits(None, None));
    }

    const SHELVES: &str = r#"
        type Query { shelves(sort: String): [Shelf!]! }
        interface Shelf { id: ID!, items: [Item!]! }
        type Fiction implements Shelf { id: ID!, items: [Item!]! }
        type Science implements Shelf { id: ID!, items: [Item!]! }
        type Poetry implements Shelf { id: ID!, items: [Item!]! }
        type Travel implements Shelf { id: ID!, items: [Item!]! }
        interface Item { id: ID! }
        type Book implements Item { id: ID!, title: String }
    "#;

    fn shelves() -> ValidFederationSchema {
        let schema = Schema::parse_and_validate(SHELVES, "schema.graphql").unwrap();
        ValidFederationSchema::new(schema).unwrap()
    }

    /// The type conditions of every inline fragment at every depth, in document order.
    fn fragments(selections: &[Selection]) -> Vec<String> {
        let mut out = Vec::new();
        for selection in selections {
            match selection {
                Selection::Field(field) => out.extend(fragments(&field.selection_set.selections)),
                Selection::InlineFragment(fragment) => {
                    out.push(
                        fragment
                            .type_condition
                            .as_ref()
                            .map(|name| name.to_string())
                            .unwrap_or_default(),
                    );
                    out.extend(fragments(&fragment.selection_set.selections));
                }
                Selection::FragmentSpread(_) => {}
            }
        }
        out
    }

    /// `shelves.@|[Fiction,Science,Poetry].items.@`: a fetch mounted into the items of three of the
    /// four kinds of shelf.
    fn conditioned_path() -> Vec<FetchDataPathElement> {
        vec![
            FetchDataPathElement::Key(name!("shelves"), None),
            FetchDataPathElement::AnyIndex(Some(vec![
                name!("Fiction"),
                name!("Science"),
                name!("Poetry"),
            ])),
            FetchDataPathElement::Key(name!("items"), None),
            FetchDataPathElement::AnyIndex(None),
        ]
    }

    /// What the fetch mounts at each item: `... on Book { title }`.
    fn book_title(schema: &ValidFederationSchema) -> Vec<Selection> {
        under(
            &selections(schema, "{ shelves { items { ... on Book { title } } } }"),
            "items",
        )
    }

    /// The path's condition names three kinds of shelf and the fetch wrote a fragment on each: a
    /// field found under each fragment is mounted once, under that fragment alone, which states the
    /// narrowing. A later fetch mounted at the same path into those copies alone finds one copy per
    /// fragment again.
    ///
    /// A control of changed behaviour: guarding the field again by every type of the condition
    /// mounts nine copies, six under a type contradicting the fragment enclosing it.
    #[test]
    fn a_field_under_a_fragment_on_a_narrowed_type_is_mounted_once() {
        let schema = shelves();
        let available = selections(
            &schema,
            "{ shelves { ... on Fiction { items { id } } ... on Science { items { id } } \
             ... on Poetry { items { id } } } }",
        );
        let mounted = book_title(&schema);
        let path = conditioned_path();
        let one_per_fragment = ["Fiction", "Book", "Science", "Book", "Poetry", "Book"];

        let first = selections_at(&schema, &available, None, &path, &mounted);
        assert_eq!(fragments(&first), one_per_fragment);

        let second = selections_at(&schema, &first, None, &path, &mounted);
        assert_eq!(fragments(&second), one_per_fragment);
    }

    /// Under a fragment wider than the condition, one reaching a kind of shelf the path excludes,
    /// a field found inside is guarded by each type of the condition the fragment reaches.
    ///
    /// A control of preserved behaviour: such a fragment states nothing, so the field is guarded
    /// as it is without one.
    #[test]
    fn a_bare_field_under_a_wider_fragment_is_narrowed() {
        let schema = shelves();
        let available = selections(&schema, "{ shelves { ... on Shelf { items { id } } } }");
        let mounted = book_title(&schema);
        let reproduced = selections_at(&schema, &available, None, &conditioned_path(), &mounted);
        assert_eq!(
            fragments(&reproduced),
            [
                "Shelf", "Fiction", "Book", "Science", "Book", "Poetry", "Book"
            ]
        );
    }

    /// Each selection on one line, which is how a test compares reproduced copies by content.
    fn printed(selections: &[Selection]) -> Vec<String> {
        selections
            .iter()
            .map(|selection| selection.serialize().no_indent().to_string())
            .collect()
    }

    /// `items` selecting `selected`, under a fragment on each kind of shelf the condition names.
    fn on_each_kind(selected: &str) -> String {
        ["Fiction", "Science", "Poetry"]
            .map(|kind| format!("... on {kind} {{ items {{ {selected} }} }}"))
            .join(" ")
    }

    /// Six copies of `shelves`, `kinds` what the first selects and `any_shelf` what the second
    /// selects under `... on Shelf`. Each operation is parsed alone, as each fetch's own is:
    /// copies of one response key that differ in an argument cannot share an operation.
    fn shelves_copies(
        schema: &ValidFederationSchema,
        kinds: &str,
        any_shelf: &str,
    ) -> Vec<Selection> {
        [
            format!("{{ shelves {{ {kinds} }} shelves {{ ... on Shelf {{ {any_shelf} }} }} }}"),
            format!(
                "query($v: Boolean!) {{ shelves @include(if: $v) {{ {kinds} }} \
                 shelves {{ ... on Shelf @include(if: $v) {{ {any_shelf} }} }} }}"
            ),
            format!("{{ shelves(sort: \"title\") {{ {kinds} }} }}"),
            format!("{{ shelves(sort: \"author\") {{ {kinds} }} }}"),
        ]
        .iter()
        .flat_map(|operation| selections(schema, operation))
        .collect()
    }

    /// A chain of fetches at one site. Fetches before it selected `shelves` six ways:
    /// - with a fragment on each kind of shelf the condition names, and with one on `Shelf`, which
    ///   select the same items;
    /// - each of those again with an `@include`, the first on the field, the second on its
    ///   fragment;
    /// - the first with a `sort` argument, given two values.
    ///
    /// Three fetches are then mounted at the same path in turn, each into everything fetched
    /// before it. Every fetch leaves its own copy of the path, and each later one finds those
    /// copies beside the earlier ones, which reproduce as they do; a reproduced copy repeating one
    /// already reproduced at its level is kept once. So every mount yields the six copies the six
    /// selections give, in their order: each differs from another in a guard, a directive or an
    /// argument alone, so all are kept.
    ///
    /// A control of changed behaviour: keeping every copy, the second mount yields each copy twice
    /// and the third four times.
    #[test]
    fn a_chain_of_fetches_at_one_site_is_reproduced_once_per_distinct_copy() {
        let schema = shelves();
        let mounted = book_title(&schema);
        let path = conditioned_path();
        let titles = on_each_kind("... on Book { title }");
        let expected = printed(&shelves_copies(&schema, &titles, &titles));

        let mut buffer = shelves_copies(&schema, &on_each_kind("id"), "items { id }");
        let mut mounts = Vec::new();
        for _ in 0..3 {
            let reproduced = selections_at(&schema, &buffer, None, &path, &mounted);
            mounts.push(printed(&reproduced));
            buffer.extend(reproduced);
        }
        assert_eq!(mounts, vec![expected; 3]);
    }
}
