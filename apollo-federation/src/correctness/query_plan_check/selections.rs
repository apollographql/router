//! Building the selection set a plan fetches.
//!
//! Port of the structural helpers in `Apollo/Definitions/QueryPlanCompleteness.lean`. A query plan
//! says *where* each fetch runs — a flatten path of response keys — and *what* it selects there.
//! Turning that back into one operation means mounting each fetch's selections at its path, which
//! is what [`selections_at`] does.
//!
//! A fetch data path names **response keys, not fields**, so the field a path element stands for
//! can only be recovered by following the path through what has already been fetched. A key may
//! also be reached by more than one field, so following it reproduces *every* copy.
//!
//! A path element's type condition names **object types, compared literally** with each object's
//! `__typename`, along the path and where it ends, as the router's `iterate_path` compares them: an
//! interface's or a union's name selects nothing, and a key that an index follows selects nothing
//! below it if it carries a condition, a list having no `__typename`. Grounding a name through its
//! possible types would credit fetches the router never runs.
//!
//! Two elements reach nothing and appear in no flatten path: `TypenameEquals` addresses data by
//! runtime type rather than by response key, and `Parent` moves up, so it cannot be followed
//! downwards. Both are used only by fetch data rewrites, whose paths `context` reads instead.
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

/// A fetch's selections with every fragment's type condition narrowed to the object types its
/// subgraph admits there, which is all the fetch can return under it.
///
/// A type condition is nominal, and the subgraph's schema, which the fetch is written in, can admit
/// fewer object types at it than the supergraph's, where the plan is compared: a member another
/// subgraph adds to a union, or an implementation it adds to an interface. Left as written, the
/// fragment would be credited in the supergraph with objects the subgraph never returns from it.
/// So a fragment on a type the supergraph admits more objects at becomes one fragment per object
/// type the subgraph admits there; one the two agree on is kept as written. A subgraph object type
/// that is a supergraph interface, an interface object, still reads as every implementation, which
/// it stands for.
///
/// `interpret_fetch_node` computes the response shape in the supergraph schema and does not do
/// this.
pub(super) fn narrow_type_conditions(
    schema: &ValidFederationSchema,
    subgraph_schema: &ValidFederationSchema,
    selections: &[Selection],
) -> Vec<Selection> {
    let mut out = Vec::with_capacity(selections.len());
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                let mut copy = (**field).clone();
                copy.selection_set.selections = narrow_type_conditions(
                    schema,
                    subgraph_schema,
                    &field.selection_set.selections,
                );
                out.push(Selection::Field(Node::new(copy)));
            }
            Selection::InlineFragment(fragment) => {
                let below = narrow_type_conditions(
                    schema,
                    subgraph_schema,
                    &fragment.selection_set.selections,
                );
                let Some(type_condition) = &fragment.type_condition else {
                    let mut copy = (**fragment).clone();
                    copy.selection_set.selections = below;
                    out.push(Selection::InlineFragment(Node::new(copy)));
                    continue;
                };
                let applied = possible_types(subgraph_schema, type_condition);
                if possible_types(schema, type_condition)
                    .iter()
                    .all(|ty| applied.contains(ty))
                {
                    let mut copy = (**fragment).clone();
                    copy.selection_set.selections = below;
                    out.push(Selection::InlineFragment(Node::new(copy)));
                } else {
                    out.extend(applied.into_iter().map(|object_type| {
                        Selection::InlineFragment(Node::new(InlineFragment {
                            type_condition: Some(object_type.clone()),
                            directives: fragment.directives.clone(),
                            selection_set: selection_set(object_type, below.clone()),
                        }))
                    }));
                }
            }
            // Spreads are inlined before a fetch's selections are read.
            Selection::FragmentSpread(_) => out.push(selection.clone()),
        }
    }
    out
}

/// Guards selections by the runtime types a path element narrowed the position to, so that a
/// requirement is mounted only where the fetch runs.
/// - A selection written under a fragment already carries its own condition; this is for the
///   unguarded case, where the narrowing would otherwise be lost.
/// - Several types are a disjunction, so each takes its own copy. Only an object name takes one:
///   the router compares each name literally with an object's `__typename`, so an abstract name
///   selects nothing.
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
        .filter(|type_name| is_object_type(schema, type_name))
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
    let directives = condition
        .iter()
        .map(|literal| literal.to_directive().into())
        .collect();
    under_directives(&ast::DirectiveList(directives), parent_type, selections)
}

/// Guards a selection set by a list of directives, one inline fragment with no type condition per
/// directive, nested, the first outermost.
///
/// `@skip` and `@include` are not repeatable, so one fragment holding two of either is invalid; the
/// planner nests one fragment per condition in force (`wrap_selection_with_type_and_conditions`),
/// which reads the same, every directive having to admit the selections. No directives means no
/// guard, and an empty selection set stays empty rather than becoming an empty guarded fragment.
pub(super) fn under_directives(
    directives: &ast::DirectiveList,
    parent_type: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    directives
        .iter()
        .rev()
        .fold(selections, |selections, directive| {
            wrap_non_empty(
                |selections| {
                    Selection::InlineFragment(Node::new(InlineFragment {
                        type_condition: None,
                        directives: ast::DirectiveList(vec![directive.clone()]),
                        selection_set: selection_set(parent_type.clone(), selections),
                    }))
                },
                selections,
            )
        })
}

/// An inline fragment on `type_condition` under `directives`, the first on it and the rest nested
/// inside ([`under_directives`]), as `wrap_selection_with_type_and_conditions` writes one.
pub(super) fn inline_fragment_under(
    type_condition: Option<&Name>,
    parent_type: &Name,
    directives: &ast::DirectiveList,
    selections: Vec<Selection>,
) -> Selection {
    let ty = type_condition.unwrap_or(parent_type).clone();
    let (first, rest) = match directives.split_first() {
        Some((first, rest)) => (
            ast::DirectiveList(vec![first.clone()]),
            ast::DirectiveList(rest.to_vec()),
        ),
        None => (ast::DirectiveList::default(), ast::DirectiveList::default()),
    };
    Selection::InlineFragment(Node::new(InlineFragment {
        type_condition: type_condition.cloned(),
        directives: first,
        selection_set: selection_set(ty.clone(), under_directives(&rest, &ty, selections)),
    }))
}

/// `selections` with fragments that say nothing removed: a fragment with no directives on the
/// type of the fragment directly enclosing it is spliced into it, and adjacent sibling fragments
/// on one type under the same directives are merged into the first. Neither changes what is
/// selected, under which types and conditions, or in which order, so inclusion reads the result as
/// it reads `selections`; it only has fewer fragments to read. Only adjacent fragments are merged,
/// since field collection takes the first field of a response key as the group's representative.
pub(super) fn collapse_fragments(selections: Vec<Selection>) -> Vec<Selection> {
    collapse_under(None, selections)
}

fn collapse_under(enclosing: Option<&Name>, selections: Vec<Selection>) -> Vec<Selection> {
    let mut out: Vec<Selection> = Vec::with_capacity(selections.len());
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                let mut copy = (*field).clone();
                copy.selection_set.selections =
                    collapse_under(None, std::mem::take(&mut copy.selection_set.selections));
                out.push(Selection::Field(Node::new(copy)));
            }
            Selection::InlineFragment(fragment) => {
                let mut copy = (*fragment).clone();
                let inner = collapse_under(
                    copy.type_condition.as_ref(),
                    std::mem::take(&mut copy.selection_set.selections),
                );
                if copy.directives.is_empty()
                    && copy.type_condition.is_some()
                    && copy.type_condition.as_ref() == enclosing
                {
                    out.extend(inner);
                    continue;
                }
                if let Some(Selection::InlineFragment(previous)) = out.last_mut()
                    && previous.type_condition == copy.type_condition
                    && previous.directives == copy.directives
                {
                    previous.make_mut().selection_set.selections.extend(inner);
                    continue;
                }
                copy.selection_set.selections = inner;
                out.push(Selection::InlineFragment(Node::new(copy)));
            }
            Selection::FragmentSpread(_) => out.push(selection),
        }
    }
    out
}

/// A `requires` entry's `@skip`/`@include` conditions as the directives they stand for.
pub(super) fn condition_directives(
    conditions: &[crate::query_plan::requires_selection::Condition],
) -> ast::DirectiveList {
    use crate::query_plan::requires_selection::ConditionValue;
    ast::DirectiveList(
        conditions
            .iter()
            .map(|condition| {
                let value = match &condition.if_ {
                    ConditionValue::Boolean(value) => ast::Value::Boolean(*value),
                    ConditionValue::Variable(name) => ast::Value::Variable(name.clone()),
                };
                Node::new(ast::Directive {
                    name: condition.kind.directive_name(),
                    arguments: vec![Node::new(ast::Argument {
                        name: apollo_compiler::name!("if"),
                        value: Node::new(value),
                    })],
                })
            })
            .collect(),
    )
}

pub(super) fn selection_set(ty: Name, selections: Vec<Selection>) -> SelectionSet {
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
    // The path's condition names object types, compared literally with each object's `__typename`
    // as the router's `iterate_path` compares them; the fragment's own condition may be abstract.
    // So the question is whether one of the names is an object type the fragment reaches -- a name
    // that is no object type, an interface's or a union's, admits nothing.
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
pub(super) fn type_includes_object(
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

/// Is `type_name` an object type of the schema?
fn is_object_type(schema: &ValidFederationSchema, type_name: &Name) -> bool {
    matches!(
        schema.schema().types.get(type_name),
        Some(apollo_compiler::schema::ExtendedType::Object(_))
    )
}

/// What a path element's narrowing comes to inside an inline fragment it admits: the names the
/// path narrowed to that the fragment reaches.
///
/// A fragment whose runtime types are all among those names states the narrowing itself, so
/// nothing is left for a field below it to be guarded by, and the field is reproduced under the
/// fragments alone. Guarding it again would add a copy per name: `n` names over `n` sibling
/// fragments would mount `n²` copies, each narrowed apart, where a reader needs one copy admitting
/// every type. A fragment with no type condition narrows nothing.
fn pending_inside(
    schema: &ValidFederationSchema,
    pending: Option<&TypeCondition>,
    fragment_type: Option<&Name>,
) -> Option<Vec<Name>> {
    let type_names = pending?;
    let Some(fragment_type) = fragment_type else {
        return Some(type_names.to_vec());
    };
    let reached = possible_types(schema, fragment_type);
    if reached.iter().all(|ty| type_names.contains(ty)) {
        None
    } else {
        Some(
            type_names
                .iter()
                .filter(|ty| reached.contains(ty))
                .cloned()
                .collect(),
        )
    }
}

/// Guards what is mounted where a path ends by the type condition of its last element, as
/// [`selections_under_key`] guards what it finds below one.
///
/// A fragment the condition admits is kept, with what is inside it guarded by what the two leave
/// between them ([`pending_inside`]); one it does not admit is dropped; an unguarded field takes
/// [`under_reached_types`]'s copies. The router's `iterate_path` selects the objects at the end of
/// a path by the same literal comparison as along it, so a fetch below a condition naming no object
/// of its position never runs. An unnarrowed end mounts what it is given, and so does one whose
/// fragments state the condition, which is how an entity fetch's selections arrive.
///
/// `interpret_plan_node_at_path` carries an element's condition to the next element and so leaves
/// the last one unread; this follows the executor instead.
fn under_type_condition(
    schema: &ValidFederationSchema,
    pending: Option<&TypeCondition>,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    let Some(type_names) = pending else {
        return selections;
    };
    let mut out = Vec::new();
    for selection in selections {
        match &selection {
            Selection::Field(_) => out.extend(under_reached_types(
                schema,
                Some(type_names),
                vec![selection],
            )),
            Selection::InlineFragment(fragment) => {
                if !type_condition_admits(
                    schema,
                    Some(type_names),
                    fragment.type_condition.as_ref(),
                ) {
                    continue;
                }
                let inside =
                    pending_inside(schema, Some(type_names), fragment.type_condition.as_ref());
                let inner = under_type_condition(
                    schema,
                    inside.as_deref(),
                    fragment.selection_set.selections.clone(),
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
            // Unreachable: spreads are inlined before a fetch's selections are mounted.
            Selection::FragmentSpread(_) => out.push(selection),
        }
    }
    out
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
        return under_type_condition(schema, pending, mounted.to_vec());
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
        // A key's condition is compared with the value it holds, and a list has no `__typename`,
        // so one carried into an index selects nothing.
        FetchDataPathElement::AnyIndex(_) if pending.is_some() => Vec::new(),
        FetchDataPathElement::AnyIndex(type_condition) => {
            selections_at(schema, available, type_condition.as_deref(), rest, mounted)
        }
        FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => Vec::new(),
    }
}

/// Every copy of a response key reachable in `available`, carrying the rest of the path.
///
/// Inline fragments admitted by `pending` are descended into and reproduced, so a reached field
/// keeps the type condition that guarded it, and `pending` also guards a field written without
/// one. Inside a fragment what is carried is what the path and the fragment leave between them
/// ([`pending_inside`]), so a field found below fragments that state the narrowing is reproduced
/// under them alone.
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
                // An unguarded field is selected on every type the path narrowed away.
                out.extend(under_reached_types(schema, pending, reproduced));
            }
            Selection::InlineFragment(fragment) => {
                if !type_condition_admits(schema, pending, fragment.type_condition.as_ref()) {
                    continue;
                }
                let inside = pending_inside(schema, pending, fragment.type_condition.as_ref());
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
        FetchDataPathElement::Key(name, _) => rename_key_under(
            schema,
            applies,
            type_filter,
            guards,
            name,
            rest,
            new_key,
            selections,
        ),
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

/// Descends past one response key, carrying the rest of the path, where `applies` admits the
/// filter against the guards in force, as `rename_at_path` filters before it descends. A new level
/// starts with no type filter and no guards, as `rename_at_path` starts with a fresh one.
#[allow(clippy::too_many_arguments)]
fn rename_key_under(
    schema: &ValidFederationSchema,
    applies: TypeFilterTest,
    type_filter: &[Name],
    guards: &[Name],
    name: &Name,
    path: &[FetchDataPathElement],
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
                let mut inner_guards = guards.to_vec();
                if let Some(type_condition) = &fragment.type_condition {
                    inner_guards.push(type_condition.clone());
                }
                copy.selection_set.selections = rename_key_under(
                    schema,
                    applies,
                    type_filter,
                    &inner_guards,
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
