//! `@context` and `@fromContext`, on both halves of correctness.
//!
//! Port of the context sections of `Apollo/Definitions/QueryPlanCompleteness.lean` and
//! `Apollo/Definitions/QueryPlanSoundness.lean`.
//!
//! A field with a `@fromContext` argument is resolved with a value taken from somewhere above it
//! in the response rather than from the client's query. The planner compiles that into two things
//! on the fetch that resolves the field: an extra argument passing a variable, and a *context
//! rewrite* — a path, written relative to where the fetch runs, and the variable to rename what is
//! found there to.
//!
//! # Completeness: the arguments are noise
//!
//! The client never wrote those arguments, so a plan fetching exactly what was asked would be
//! judged not to, on an argument mismatch. What a fetch contributes is therefore read with them
//! dropped, as `remove_context_arguments` drops them from the fetch's response shape.
//!
//! # Soundness: the value has to be there
//!
//! At run time `subgraph_context.rs` resolves a rewrite's path against the path the fetch runs at
//! and reads the data. A path reaching nothing is not an error there — the argument is simply
//! never bound — so nothing notices that the plan asked for data it never fetched. This checks it,
//! which the legacy checker does not.
//!
//! [`path_copies`] follows a path and returns one entry per copy of what it addresses, holding
//! what guards that copy. The obligation compares two of those — the value's, and the fetch's own
//! position, which [`fetch_positions`] follows into the object the fetch runs on and narrows to
//! the types its `requires` entries are written at, the router sending a representation for no
//! other. The context object is that object or an ancestor of it, so the shared prefix guards both
//! sides and cancels; what is left to check is that wherever the fetch runs, the value is there.
//!
//! The two kinds of condition are not compared the same way, because a Boolean variable is global
//! while a type condition is about one object. Boolean literals are one conjunction for the whole
//! path; type conditions are kept level by level, one [`LevelTypes`] per object the path addressed.
//!
//! The obligation is really over pairs of a runtime configuration and a Boolean assignment, and
//! the two dimensions are handled the same way: a disjunction on each. The fetch position is split
//! into the runtime configurations it can have — one type per level it shares with the context
//! path, which [`shared_levels`] counts and [`runtime_combinations`] enumerates — and each is
//! asked separately. For one configuration, [`type_levels_admit`] keeps the value copies that
//! admit the type the data has at every shared level; the fetch's own narrowing past the shared
//! levels is ignored, restricting when it runs and not where the value is.
//!
//! Past the shared levels nothing about the fetch says what the value's objects are, and a value is
//! often split into copies there, each narrowed to some of the types a level allows: a fetch writes
//! the split with a fragment per type, and one fetch per type makes it with path conditions. So
//! the value is asked for at every sequence of types its objects can have past the shared levels,
//! among those the plan returns at each field's position ([`returned_types_along`]), and for each,
//! the copies admitting those types are what is read. Copies split at several levels then serve
//! together what one copy admitting every type would, and a type no fetch returns there is owed
//! nothing. What is left is Boolean, and [`boolean_condition_covered_by`] decides that symbolically.
//!
//! A level is the object types its guards admit of those the schema allows there, so a vacuous
//! narrowing is no narrowing: a `... on T` where `T` is the only type the position can have leaves
//! the level as it was.
//!
//! One variable at a time, not one rewrite at a time. A fetch can carry several rewrites binding
//! the same variable, one per runtime type the context object may have, and the router takes the
//! value from whichever resolves — `execute_on_path` claims the variable only once a rewrite has
//! produced a value, so one that finds nothing falls through to the next.
//! [`context_value_clauses`] gathers the copies of every rewrite binding the variable before the
//! coverage test, and a rewrite whose path does not resolve contributes nothing rather than
//! failing on its own. A value is asked for only at a configuration some rewrite binding the
//! variable targets ([`context_variable_targeted`]); elsewhere the router binds the variable to
//! nothing and reads no value, and whether one is owed there is the declaration's question.
//!
//! That disjunction is what makes the runtime configurations matter. Where the query splits the
//! fetch position by runtime type, each value copy meets a position copy of its own; where the
//! position is not split — a field declared on an interface, selected without a fragment — no
//! single rewrite covers it, and only taking the types one at a time shows that between them they
//! do.
//!
//! A merged path is spliced from two kinds of path that carry their type conditions differently —
//! a flatten path on its elements, a rewrite path as `TypenameEquals` — so both are read, each
//! narrowing the level it is about.
//!
//! # Soundness: the argument has to be passed, and what it declares
//!
//! A rewrite that reaches nothing is not the only way an argument goes unbound. The router runs
//! context rewrites only for a fetch it sends representations, so a root fetch passing a
//! `@fromContext` argument passes it nothing ([`check_root_fetch_uses_no_context`]); and a fetch
//! that drops both the argument and its rewrite leaves no rewrite for availability to check, so
//! every contextual argument must be passed a variable
//! ([`check_context_arguments_pass_variables`]). Both are read off the fetch's operation in its
//! subgraph's schema ([`context_uses`]), which says which arguments are contextual.
//!
//! Nor does anything at run time compare what a rewrite reads with the declaration: a key holds
//! whatever an earlier fetch put under it. So every rewrite binding a contextual argument is read
//! back against the declaration ([`check_context_declared`]): the object it climbs to must be the
//! nearest ancestor setting the context, and the fields its keys stand for below it must read down
//! to a leaf of the declared selection, by field name rather than response key. A value may also be
//! absent where the declaration has none: it is owed where the declaration selects something at
//! the nearest setter's type, or where the fetch declares the variable non-null, and every value
//! owed must be one a rewrite targets ([`check_owed_values_targeted`]).
//!
//! A use is judged only at the objects the fetch runs on where its field is selected
//! ([`context_use_made_at`]): a fetch sent several entity types may select the contextual field
//! under one of them, and a rewrite with no type test then reads a value off the others too, which
//! no field there reads. Availability asks for a value only there too ([`needs_variable_at`]),
//! but everywhere for a non-null variable, an unbound one failing the whole fetch.

#[cfg(test)]
#[path = "context_test.rs"]
mod context_test;

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::conditions::BooleanCondition;
use super::super::query_compare::conditions::BooleanLiteral;
use super::super::query_compare::conditions::boolean_condition_covered_by;
use super::super::query_compare::conditions::canonical_boolean_condition;
use super::super::query_compare::conditions::literals_for_directives;
use super::ComparisonError;
use super::returned::ReturnedTypes;
use super::returned::TypedKeyPath;
use super::selections::TypeCondition;
use super::selections::does_fragment_type_apply;
use super::selections::field_output_type;
use super::selections::possible_types;
use super::selections::type_includes_object;
use super::subgraph::ContextArgument;
use super::subgraph::Subgraph;
use crate::operation::TYPENAME_FIELD;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Completeness: dropping the synthetic arguments
//==================================================================================================

/// The variables a fetch's context rewrites bind, which its operation passes as the synthetic
/// arguments.
///
/// `remove_context_arguments` fails on a value setter among these; the syntax admits one, so this
/// passes over it as output-rewrite handling does.
pub(super) fn context_variables(rewrites: &[Arc<FetchDataRewrite>]) -> Vec<Name> {
    rewrites
        .iter()
        .filter_map(|rewrite| match rewrite.as_ref() {
            FetchDataRewrite::KeyRenamer(renamer) => Some(renamer.rename_key_to.clone()),
            FetchDataRewrite::ValueSetter(_) => None,
        })
        .collect()
}

/// Drops the synthetic `@fromContext` arguments from every field of a selection set, at every
/// depth, as `remove_context_arguments_in_response_shape` does.
pub(super) fn remove_context_arguments(
    variables: &[Name],
    selections: Vec<Selection>,
) -> Vec<Selection> {
    if variables.is_empty() {
        return selections;
    }
    selections
        .into_iter()
        .map(|selection| match selection {
            Selection::Field(field) => {
                let mut copy = (*field).clone();
                copy.arguments
                    .retain(|argument| !is_contextual(variables, argument));
                copy.selection_set.selections = remove_context_arguments(
                    variables,
                    std::mem::take(&mut copy.selection_set.selections),
                );
                Selection::Field(Node::new(copy))
            }
            Selection::InlineFragment(fragment) => {
                let mut copy = (*fragment).clone();
                copy.selection_set.selections = remove_context_arguments(
                    variables,
                    std::mem::take(&mut copy.selection_set.selections),
                );
                Selection::InlineFragment(Node::new(copy))
            }
            selection => selection,
        })
        .collect()
}

/// Does an argument pass one of the context variables, and so was added for `@fromContext` rather
/// than written by the client?
fn is_contextual(variables: &[Name], argument: &Node<ast::Argument>) -> bool {
    match &argument.value.as_ref() {
        ast::Value::Variable(name) => variables.contains(name),
        _ => false,
    }
}

//==================================================================================================
// Soundness: resolving a rewrite's path
//==================================================================================================

/// Goes up one level of a fetch's path, taking that path reversed: trailing elements are dropped
/// until a response key has been dropped, since an index does not name a level of the query.
/// `None` is `merge_context_path`'s `InvalidRelativePath` — the path ran out.
fn up_one_level(reversed: &[FetchDataPathElement]) -> Option<&[FetchDataPathElement]> {
    let (head, rest) = reversed.split_first()?;
    match head {
        FetchDataPathElement::Key(_, _) => Some(rest),
        _ => up_one_level(rest),
    }
}

/// Resolves a context rewrite's path against the path the fetch runs at, as `merge_context_path`
/// does: each leading `Parent` goes up one level of the fetch's path, and what is left of the
/// context path is appended to what is left of the fetch's.
///
/// The router spells `Parent` as a key named `..` and stops eating at the first element that is
/// not one; a `Parent` elsewhere in the path is carried along and reaches nothing, as here.
fn merge_context_path(
    path: &[FetchDataPathElement],
    context_path: &[FetchDataPathElement],
) -> Option<Vec<FetchDataPathElement>> {
    let mut reversed: Vec<FetchDataPathElement> = path.iter().rev().cloned().collect();
    let mut context_path = context_path;
    while let Some((FetchDataPathElement::Parent, rest)) = context_path.split_first() {
        reversed = up_one_level(&reversed)?.to_vec();
        context_path = rest;
    }
    reversed.reverse();
    reversed.extend(context_path.iter().cloned());
    Some(reversed)
}

/// How many levels two paths address in common: the root, and one per response key they agree on
/// from there. A key's own type condition does not enter into it, a rewrite path carrying none.
fn shared_levels(left: &[FetchDataPathElement], right: &[FetchDataPathElement]) -> usize {
    match (left.split_first(), right.split_first()) {
        (
            Some((FetchDataPathElement::Key(left_name, _), left_rest)),
            Some((FetchDataPathElement::Key(right_name, _), right_rest)),
        ) => {
            if left_name == right_name {
                1 + shared_levels(left_rest, right_rest)
            } else {
                1
            }
        }
        (Some((FetchDataPathElement::AnyIndex(_), left_rest)), _) => {
            shared_levels(left_rest, right)
        }
        (_, Some((FetchDataPathElement::AnyIndex(_), right_rest))) => {
            shared_levels(left, right_rest)
        }
        (Some((FetchDataPathElement::TypenameEquals(_), left_rest)), _) => {
            shared_levels(left_rest, right)
        }
        (_, Some((FetchDataPathElement::TypenameEquals(_), right_rest))) => {
            shared_levels(left, right_rest)
        }
        _ => 1,
    }
}

//==================================================================================================
// Soundness: under what conditions a path reaches a value
//==================================================================================================

/// The object types an object at one level of a path can have: those the schema allows there,
/// narrowed by the fragments and `TypenameEquals` in force.
type LevelTypes = Vec<Name>;

/// The level a path starts at, and the one a field's selection set starts at: every type the
/// position can have, nothing narrowing it yet.
fn level_of(schema: &ValidFederationSchema, parent_type: &Name) -> LevelTypes {
    possible_types(schema, parent_type)
}

/// Narrows a level to one type condition of a fragment or a `TypenameEquals`.
fn narrow_by(schema: &ValidFederationSchema, level: &[Name], type_name: &Name) -> LevelTypes {
    narrowed_to(level, &possible_types(schema, type_name))
}

/// Narrows a level by a path element's own type condition, a disjunction of object names compared
/// literally with each object's `__typename`, as the router's `iterate_path` compares them: the
/// level keeps the types it holds that the condition names. A name that is no object type, an
/// interface's or a union's, keeps nothing, so a value read below it is read nowhere.
fn narrow_by_condition(level: &[Name], type_condition: Option<&TypeCondition>) -> LevelTypes {
    match type_condition {
        None => level.to_vec(),
        Some(type_names) => narrowed_to(level, type_names),
    }
}

fn narrowed_to(level: &[Name], admitted: &[Name]) -> LevelTypes {
    level
        .iter()
        .filter(|type_name| admitted.contains(type_name))
        .cloned()
        .collect()
}

/// One copy of what a path reaches in fetched data: the types it can have at each level, the
/// Boolean guard over it, and the fields it was reached by.
///
/// Type conditions are kept level by level, each being about the object at one level; Boolean
/// literals are global, so they are one conjunction for the whole path.
#[derive(Clone)]
pub(super) struct ReachedCopy {
    /// The levels already descended past, outermost first.
    levels: Vec<LevelTypes>,
    /// The level the copy sits at.
    current: LevelTypes,
    condition: BooleanCondition,
    /// The field each key the path descended past, or ended at, stands for, outermost first,
    /// whatever its response key.
    field_path: Vec<Name>,
}

impl ReachedCopy {
    /// Extends a copy's condition by the literals one selection's directives stand for, kept
    /// canonical.
    ///
    /// `None` when they can never all hold, which is no copy at all: `literals_for_directives`
    /// rejects a directive that is false outright, and canonicalization a literal that meets its
    /// complement further up the path.
    fn under(&self, directives: &ast::DirectiveList) -> Option<ReachedCopy> {
        let literals = literals_for_directives(directives)?;
        let mut combined = self.condition.clone();
        combined.extend(literals);
        Some(ReachedCopy {
            levels: self.levels.clone(),
            current: self.current.clone(),
            condition: canonical_boolean_condition(&combined)?,
            field_path: self.field_path.clone(),
        })
    }

    /// The types of a copy, one entry per level of the path, outermost first.
    fn type_levels(&self) -> Vec<&LevelTypes> {
        self.levels
            .iter()
            .chain(std::iter::once(&self.current))
            .collect()
    }

    /// Descends into a field's selection set: the level just left is kept, and the one below
    /// starts from the field's own type.
    fn descend(&self, level: LevelTypes) -> Self {
        let mut descended = self.clone();
        descended.levels.push(descended.current);
        descended.current = level;
        descended
    }

    /// A fetch position with the object the fetch runs on fixed to one of its types
    /// (`ReachedCopy.atType`).
    fn at_type(&self, entity_type: &Name) -> Self {
        ReachedCopy {
            current: vec![entity_type.clone()],
            ..self.clone()
        }
    }

    /// The fields a copy's keys stand for past its first `shared` levels, each with the response
    /// key `path`, the path that reached it, names it by (`ReachedCopy.fieldsBelow`).
    fn fields_below(&self, path: &[FetchDataPathElement], shared: usize) -> Vec<(Name, Name)> {
        key_path(path)
            .into_iter()
            .zip(&self.field_path)
            .skip(shared.saturating_sub(1))
            .map(|(key, field_name)| (key.clone(), field_name.clone()))
            .collect()
    }
}

/// A path cut right after its first `count` keys (`FetchDataPath.throughKeys`).
fn through_keys(count: usize, path: &[FetchDataPathElement]) -> Vec<FetchDataPathElement> {
    let mut left = count;
    let mut out = Vec::new();
    for element in path {
        if left == 0 {
            break;
        }
        out.push(element.clone());
        if matches!(element, FetchDataPathElement::Key(_, _)) {
            left -= 1;
        }
    }
    out
}

/// The keys of a fetch data path alone (`FetchDataPath.keyPath`).
fn key_path(path: &[FetchDataPathElement]) -> Vec<&Name> {
    path.iter()
        .filter_map(|element| match element {
            FetchDataPathElement::Key(name, _) => Some(name),
            _ => None,
        })
        .collect()
}

/// What a path addresses: the value its last key holds, which a rewrite reads, or the object that
/// value is, which a fetch runs on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PathEnd {
    Value,
    Object,
}

/// The copy a path starts from: the query root, with every type it can have and unguarded.
fn root_reached(schema: &ValidFederationSchema, root_type: &Name) -> ReachedCopy {
    ReachedCopy {
        levels: Vec::new(),
        current: level_of(schema, root_type),
        condition: BooleanCondition::new(),
        field_path: Vec::new(),
    }
}

/// The copies of the value a path addresses in what the plan has already fetched: what a context
/// rewrite reads. Where the fetch itself runs is [`fetch_positions`].
pub(super) fn path_copies(
    schema: &ValidFederationSchema,
    root_type: &Name,
    available: &[Selection],
    path: &[FetchDataPathElement],
) -> Vec<ReachedCopy> {
    copies_reached(
        schema,
        root_type,
        &root_reached(schema, root_type),
        PathEnd::Value,
        path,
        available,
    )
}

/// Where an entity fetch runs: one entry per copy of the object its path addresses in what the
/// plan has already fetched, its levels running from the root to the fetch's own object.
///
/// The router sends a representation only for an object its `requires` selection yields something
/// for (`Variables::new`), so that last level is narrowed to `requires_types`, the types the
/// entries are written at, and a copy narrowed to none of them is one the fetch never runs at.
fn fetch_positions(
    schema: &ValidFederationSchema,
    root_type: &Name,
    available: &[Selection],
    path: &[FetchDataPathElement],
    requires_types: &[Name],
) -> Vec<ReachedCopy> {
    copies_reached(
        schema,
        root_type,
        &root_reached(schema, root_type),
        PathEnd::Object,
        path,
        available,
    )
    .into_iter()
    .filter_map(|mut position| {
        position.current = narrowed_to(&position.current, requires_types);
        (!position.current.is_empty()).then_some(position)
    })
    .collect()
}

/// The copies a fetch data path reaches in a selection set, stopping where `ending` says; none
/// when it addresses nothing. A `TypenameEquals` narrows the level it stands at, the rewrite
/// reading only data of that runtime type. A copy narrowed to no type at some level is dropped,
/// the router's path reader finding no object there.
fn copies_reached(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    reached: &ReachedCopy,
    ending: PathEnd,
    path: &[FetchDataPathElement],
    selections: &[Selection],
) -> Vec<ReachedCopy> {
    let Some((head, rest)) = path.split_first() else {
        if reached.current.is_empty() {
            return Vec::new();
        }
        return vec![reached.clone()];
    };
    let narrowed = |current: LevelTypes| ReachedCopy {
        current,
        ..reached.clone()
    };
    match head {
        FetchDataPathElement::Key(name, type_condition) => reached_under_key(
            schema,
            parent_type,
            reached,
            name,
            type_condition.as_deref(),
            ending,
            rest,
            selections,
        ),
        FetchDataPathElement::TypenameEquals(type_name) => copies_reached(
            schema,
            parent_type,
            &narrowed(narrow_by(schema, &reached.current, type_name)),
            ending,
            rest,
            selections,
        ),
        FetchDataPathElement::AnyIndex(type_condition) => copies_reached(
            schema,
            parent_type,
            &narrowed(narrow_by_condition(
                &reached.current,
                type_condition.as_deref(),
            )),
            ending,
            rest,
            selections,
        ),
        // `merge_context_path` resolves the leading `Parent` elements before anything is looked
        // up, so one arriving here points out of the data.
        FetchDataPathElement::Parent => Vec::new(),
    }
}

/// Every copy of a response key at this level, with the rest of the path followed into it.
///
/// A fragment narrows the level the copy sits at, and a key element's own type condition narrows
/// the level below, where the field's own type takes over. A field inside a fragment is a field of
/// the type the fragment names, which is where its type is looked up when the path descends past
/// it, a fragment naming no type keeping the enclosing one: below a union, whose members alone
/// declare the fields a path descends past, such a key is found inside the fragments on its
/// members. A selection whose directives can never all hold contributes no copy. The field a matched
/// key stands for is recorded, whether the path ends at it or descends past it. A path ending at a
/// value stops at the key that holds it; one ending at an object descends into that key's own
/// level.
#[allow(clippy::too_many_arguments)]
fn reached_under_key(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    reached: &ReachedCopy,
    name: &Name,
    type_condition: Option<&TypeCondition>,
    ending: PathEnd,
    path: &[FetchDataPathElement],
    selections: &[Selection],
) -> Vec<ReachedCopy> {
    let mut out = Vec::new();
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if field.response_name() != name || reached.current.is_empty() {
                    continue;
                }
                let Some(mut found) = reached.under(&field.directives) else {
                    continue;
                };
                found.field_path.push(field.name.clone());
                if ending == PathEnd::Value && path.is_empty() {
                    out.push(found);
                    continue;
                }
                let Some(child_type) = field_output_type(schema, parent_type, &field.name) else {
                    continue;
                };
                let level = narrow_by_condition(&level_of(schema, &child_type), type_condition);
                out.extend(copies_reached(
                    schema,
                    &child_type,
                    &found.descend(level),
                    ending,
                    path,
                    &field.selection_set.selections,
                ));
            }
            Selection::InlineFragment(fragment) => {
                let narrowed = match &fragment.type_condition {
                    Some(fragment_type) => narrow_by(schema, &reached.current, fragment_type),
                    None => reached.current.clone(),
                };
                if narrowed.is_empty() {
                    continue;
                }
                let Some(mut inner) = reached.under(&fragment.directives) else {
                    continue;
                };
                inner.current = narrowed;
                out.extend(reached_under_key(
                    schema,
                    fragment.type_condition.as_ref().unwrap_or(parent_type),
                    &inner,
                    name,
                    type_condition,
                    ending,
                    path,
                    &fragment.selection_set.selections,
                ));
            }
            // Spreads are inlined before a fetch's selections join the buffer.
            Selection::FragmentSpread(_) => {}
        }
    }
    out
}

/// Do a copy's levels admit the types the data has at its first levels, outermost first: does each
/// of those levels admit its type? Levels past them do not enter into it.
fn type_levels_admit(value: &[&LevelTypes], runtime_types: &[Name]) -> bool {
    value
        .iter()
        .zip(runtime_types)
        .all(|(level, type_name)| level.contains(type_name))
}

/// Every runtime configuration a fetch position can have over the levels it shares with a context
/// path: one type per level, drawn from what that level admits, outermost first.
fn runtime_combinations(shared: usize, levels: &[&LevelTypes]) -> Vec<Vec<Name>> {
    let (Some(shared), Some((level, rest))) = (shared.checked_sub(1), levels.split_first()) else {
        return vec![Vec::new()];
    };
    runtime_combinations(shared, rest)
        .into_iter()
        .flat_map(|combination| {
            level.iter().map(move |type_name| {
                let mut extended = vec![type_name.clone()];
                extended.extend(combination.iter().cloned());
                extended
            })
        })
        .collect()
}

/// How many levels of the fetch's position the rewrites binding one variable can see: the most any
/// of them shares with it. A rewrite that shares fewer reads only that many.
fn context_shared_levels(
    path: &[FetchDataPathElement],
    context_rewrites: &[Arc<FetchDataRewrite>],
    variable_name: &Name,
) -> usize {
    context_rewrites
        .iter()
        .filter_map(|rewrite| match rewrite.as_ref() {
            FetchDataRewrite::KeyRenamer(renamer) if renamer.rename_key_to == *variable_name => {
                merge_context_path(path, &renamer.path)
            }
            _ => None,
        })
        .map(|merged| shared_levels(&merged, path))
        .max()
        .unwrap_or(0)
}

//==================================================================================================
// Soundness: the obligation
//==================================================================================================

/// Do a path's own type tests admit the types the data has at the levels it addresses, outermost
/// first (`FetchDataPath.admitsRuntimeTypes`)? A `TypenameEquals` tests the type at its level, and
/// a key's or an index's condition the type at the level it is about. Levels past the given types
/// are not tested.
fn path_admits_runtime_types(
    schema: &ValidFederationSchema,
    path: &[FetchDataPathElement],
    runtime_types: &[Name],
) -> bool {
    let (Some((element, rest)), Some((object_type, below))) =
        (path.split_first(), runtime_types.split_first())
    else {
        return true;
    };
    match element {
        FetchDataPathElement::Key(_, type_condition) => {
            (match (type_condition.as_deref(), below.first()) {
                (Some(type_names), Some(next)) => type_names.contains(next),
                _ => true,
            }) && path_admits_runtime_types(schema, rest, below)
        }
        FetchDataPathElement::TypenameEquals(type_name) => {
            possible_types(schema, type_name).contains(object_type)
                && path_admits_runtime_types(schema, rest, runtime_types)
        }
        FetchDataPathElement::AnyIndex(type_condition) => {
            type_condition
                .as_deref()
                .is_none_or(|type_names| type_names.contains(object_type))
                && path_admits_runtime_types(schema, rest, runtime_types)
        }
        FetchDataPathElement::Parent => false,
    }
}

/// Is a contextual use whose field's object and ancestors in the fetch's operation have the types
/// `use_levels` made at an object the fetch runs on of type `entity_type` (`contextUseMadeAt`)? A
/// type at the level below the root, the object the fetch runs on, includes it in the supergraph,
/// as an interface object includes each implementation it stands for. A use at the root is made
/// wherever the fetch runs.
fn context_use_made_at(
    schema: &ValidFederationSchema,
    use_levels: &[Vec<Name>],
    entity_type: &Name,
) -> bool {
    use_levels.get(1).is_none_or(|level| {
        level
            .iter()
            .any(|use_type| type_includes_object(schema, use_type, entity_type))
    })
}

/// Does a fetch need a value for `variable` at an object it runs on of type `entity_type`
/// (`FetchNode.NeedsVariableAt`)? Everywhere where its operation declares the variable non-null,
/// an unbound one failing the fetch; else where it passes the variable to a contextual argument
/// whose use is made at that object ([`context_use_made_at`]).
fn needs_variable_at(
    schema: &ValidFederationSchema,
    fetch: &FetchNode,
    uses: &[ContextUse],
    variable: &Name,
    entity_type: &Name,
) -> bool {
    declares_variable_non_null(fetch, variable)
        || uses.iter().any(|found| {
            found.variable.as_ref() == Some(variable)
                && context_use_made_at(schema, &found.levels, entity_type)
        })
}

/// Does some rewrite binding a variable target a runtime configuration of the fetch's position
/// (`contextVariableTargeted`): does its path, resolved against the fetch's, admit the
/// configuration's types at the levels the two share? Where none does, the router binds the variable
/// to nothing, and no value is read.
fn context_variable_targeted(
    schema: &ValidFederationSchema,
    path: &[FetchDataPathElement],
    context_rewrites: &[Arc<FetchDataRewrite>],
    runtime_types: &[Name],
    variable_name: &Name,
) -> bool {
    context_rewrites.iter().any(|rewrite| {
        let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
            return false;
        };
        renamer.rename_key_to == *variable_name
            && merge_context_path(path, &renamer.path).is_some_and(|merged| {
                let shared = shared_levels(&merged, path).min(runtime_types.len());
                path_admits_runtime_types(schema, &merged, &runtime_types[..shared])
            })
    })
}

/// The copies of a context value one runtime configuration of the fetch's position can read:
/// those whose levels admit it over the levels the merged path shares with the fetch's
/// (`contextReadCopies`). Which of them a run reads depends also on the types the value's own
/// objects have below those levels ([`returned_types_along`]). `copies` is [`path_copies`] of
/// `merged`.
fn context_read_copies<'c>(
    copies: &'c [ReachedCopy],
    path: &[FetchDataPathElement],
    merged: &[FetchDataPathElement],
    runtime_types: &[Name],
) -> impl Iterator<Item = &'c ReachedCopy> {
    let shared = shared_levels(merged, path);
    let configuration = runtime_types[..runtime_types.len().min(shared)].to_vec();
    copies
        .iter()
        .filter(move |copy| type_levels_admit(&copy.type_levels(), &configuration))
}

/// Every sequence of object types the fields `fields`, each with its response key, can return in
/// turn, followed from an object of type `object_type` below the typed key path `typed_key_path`,
/// one type per field, as the plan returns them at each field's position (`returnedTypesAlong`):
/// none past a field that returns no type.
fn returned_types_along(
    schema: &ValidFederationSchema,
    returned: &ReturnedTypes,
    typed_key_path: &TypedKeyPath,
    object_type: &Name,
    fields: &[(Name, Name)],
) -> Vec<Vec<Name>> {
    let Some(((key, field_name), rest)) = fields.split_first() else {
        return vec![Vec::new()];
    };
    let mut field_path = typed_key_path.clone();
    field_path.push((key.clone(), Some(vec![object_type.clone()])));
    // `returnedChildTypes`: the field's types in the supergraph that a fetch returns there.
    let nodes = returned.written_at_position(&field_path);
    let result_types: Vec<Name> = field_output_type(schema, object_type, field_name)
        .map(|child_type| possible_types(schema, &child_type))
        .unwrap_or_default()
        .into_iter()
        .filter(|child| returned.returns_at(&nodes, object_type, field_name, child))
        .collect();
    result_types
        .iter()
        .flat_map(|result_type| {
            returned_types_along(schema, returned, &field_path, result_type, rest)
                .into_iter()
                .map(move |mut types| {
                    types.insert(0, result_type.clone());
                    types
                })
        })
        .collect()
}

/// One element of each list, in every way (`choices`).
fn choices<T: Clone>(options: &[Vec<T>]) -> Vec<Vec<T>> {
    let Some((first, rest)) = options.split_first() else {
        return vec![Vec::new()];
    };
    let rest = choices(rest);
    first
        .iter()
        .flat_map(|option| {
            rest.iter().map(move |chosen| {
                let mut choice = vec![option.clone()];
                choice.extend(chosen.iter().cloned());
                choice
            })
        })
        .collect()
}

/// The conditions under which one rewrite finds its value, for one runtime configuration of the
/// fetch's position (`contextRewriteClauses`): one list per key it must find, holding the
/// conditions of the copies it reaches there ([`context_read_copies`]). It must find the first key
/// past the levels it shares with the fetch, and then, under every sequence of types the fields
/// found so far can return ([`returned_types_along`]), the next key, down to the value's own. A
/// field the plan returns no type for ends the search there, its value being null. The shared
/// levels are read at the configuration's types.
#[allow(clippy::too_many_arguments)]
fn context_rewrite_clauses(
    schema: &ValidFederationSchema,
    root_type: &Name,
    returned: &ReturnedTypes,
    path: &[FetchDataPathElement],
    readable: &[Selection],
    merged: &[FetchDataPathElement],
    runtime_types: &[Name],
) -> Vec<Vec<BooleanCondition>> {
    let shared = shared_levels(merged, path);
    let keys_below = fetch_path_depth(merged).saturating_sub(shared.saturating_sub(1));
    let configuration = &runtime_types[..runtime_types.len().min(shared)];
    let object_type = configuration.last().unwrap_or(root_type);
    let typed_key_path: TypedKeyPath = key_path(merged)
        .into_iter()
        .zip(&configuration[..configuration.len().saturating_sub(1)])
        .map(|(key, type_name)| (key.clone(), Some(vec![type_name.clone()])))
        .collect();
    // The copies the rewrite reaches at its `depth`-th key past the shared levels.
    let reached_at = |depth: usize| -> Vec<ReachedCopy> {
        let read = if keys_below <= depth {
            merged.to_vec()
        } else {
            through_keys(shared.saturating_sub(1) + depth, merged)
        };
        let copies = path_copies(schema, root_type, readable, &read);
        context_read_copies(&copies, path, &read, runtime_types)
            .cloned()
            .collect()
    };
    let conditions_at = |reached: &[ReachedCopy], types: &[Name]| -> Vec<BooleanCondition> {
        reached
            .iter()
            .filter(|copy| {
                let levels = copy.type_levels();
                type_levels_admit(&levels[shared.min(levels.len())..], types)
            })
            .map(|copy| copy.condition.clone())
            .collect()
    };
    let mut clauses = vec![conditions_at(&reached_at(keys_below.min(1)), &[])];
    for depth in 1..keys_below {
        let next = reached_at(depth + 1);
        for copy in reached_at(depth) {
            for types in returned_types_along(
                schema,
                returned,
                &typed_key_path,
                object_type,
                &copy.fields_below(merged, shared),
            ) {
                clauses.push(conditions_at(&next, &types));
            }
        }
    }
    clauses
}

/// When a fetch finds the value bound to one context variable, for one runtime configuration of its
/// position (`contextValueClauses`): lists of conditions, each of which must cover the fetch's
/// condition. The router takes the value from whichever rewrite binding the variable finds one, and
/// a rewrite finds it when it finds every key it must ([`context_rewrite_clauses`]); so for each way
/// of picking one such key of each rewrite, the conditions of finding them are pooled into one list
/// ([`choices`]). With no rewrite, the single list is empty.
#[allow(clippy::too_many_arguments)]
fn context_value_clauses(
    schema: &ValidFederationSchema,
    root_type: &Name,
    returned: &ReturnedTypes,
    path: &[FetchDataPathElement],
    readable: &[Selection],
    context_rewrites: &[Arc<FetchDataRewrite>],
    runtime_types: &[Name],
    variable_name: &Name,
) -> Vec<Vec<BooleanCondition>> {
    let per_rewrite: Vec<Vec<Vec<BooleanCondition>>> = context_rewrites
        .iter()
        .filter_map(|rewrite| {
            let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
                return None;
            };
            if renamer.rename_key_to != *variable_name {
                return None;
            }
            let merged = merge_context_path(path, &renamer.path)?;
            Some(context_rewrite_clauses(
                schema,
                root_type,
                returned,
                path,
                readable,
                &merged,
                runtime_types,
            ))
        })
        .collect();
    choices(&per_rewrite)
        .into_iter()
        .map(|choice| choice.into_iter().flatten().collect())
        .collect()
}

/// Is every contextual value a fetch reads there wherever the fetch runs, needs it
/// ([`needs_variable_at`]) and a rewrite binding its variable targets it
/// ([`context_variable_targeted`]), whatever types the value's objects have below the levels it
/// shares with the fetch, among those the plan returns? A value no field reads goes unused.
///
/// Deciding this per copy of the fetch's own position is what makes the shared prefix cancel; see
/// the module docs.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_context_rewrites(
    schema: &ValidFederationSchema,
    root_type: &Name,
    returned: &ReturnedTypes,
    uses: &[ContextUse],
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &[Selection],
    readable: &[Selection],
    fetch: &FetchNode,
    requires_types: &[Name],
) -> Result<(), ComparisonError> {
    let mut variables = context_variables(&fetch.context_rewrites);
    // Several rewrites can bind one variable; the check is the same for each, so ask it once.
    variables.dedup();
    if variables.is_empty() {
        return Ok(());
    }
    // Where the fetch runs is found in everything fetched before it; the values it reads, in what
    // it can read. The two differ only inside a deferred block.
    let positions = fetch_positions(schema, root_type, available, path, requires_types);
    for variable in &variables {
        let shared = context_shared_levels(path, &fetch.context_rewrites, variable);
        for position in &positions {
            let mut running = condition.to_vec();
            running.extend(position.condition.iter().cloned());
            // A position the fetch can never run at is met by nothing at all: asking for it to be
            // covered would reject a plan over data that does not exist.
            let Some(reachable) = canonical_boolean_condition(&running) else {
                continue;
            };
            let at_types = position.current.iter().filter(|entity_type| {
                needs_variable_at(schema, fetch, uses, variable, entity_type)
            });
            for at_type in at_types.map(|entity_type| position.at_type(entity_type)) {
                for runtime_types in runtime_combinations(shared, &at_type.type_levels()) {
                    // Where no rewrite binding the variable targets the configuration, the router binds
                    // it to nothing and no value is read; whether one is owed is the declaration's
                    // question ([`check_context_declared`]).
                    if !context_variable_targeted(
                        schema,
                        path,
                        &fetch.context_rewrites,
                        &runtime_types,
                        variable,
                    ) {
                        continue;
                    }
                    let all_clauses = context_value_clauses(
                        schema,
                        root_type,
                        returned,
                        path,
                        readable,
                        &fetch.context_rewrites,
                        &runtime_types,
                        variable,
                    );
                    if all_clauses
                        .iter()
                        .any(|covers| !boolean_condition_covered_by(&reachable, covers))
                    {
                        return Err(ComparisonError::new(format!(
                            "fetch to {}: the contextual value for `${variable}` is not always fetched \
                         where the fetch runs\n* fetch runs at: {}\n* for data of: {}",
                            fetch.subgraph_name,
                            display_path(path),
                            runtime_types
                                .iter()
                                .map(|type_name| type_name.as_str())
                                .collect::<Vec<_>>()
                                .join(" / "),
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

//==================================================================================================
// Contextual arguments: bound by an entity fetch, passed by no other
//==================================================================================================

/// A `@fromContext` argument a fetch's operation passes: the field it is an argument of, its
/// declaration, and the variable the operation passes it, if it passes one (the model's
/// `ContextUse`), with the types the field's object and each of its ancestors can have in the
/// operation, one list per level from its root, outermost first, the field's object last.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ContextUse {
    parent_type: Name,
    field_name: Name,
    declaration: ContextArgument,
    variable: Option<Name>,
    levels: Vec<Vec<Name>>,
}

impl ContextUse {
    fn describe(&self) -> String {
        format!(
            "`{}.{}`'s `@fromContext` argument `{}`",
            self.parent_type, self.field_name, self.declaration.argument_name
        )
    }
}

/// The variable an argument is passed, read at its first occurrence, which validity makes its only
/// one; `None` for a literal or an argument left out.
fn argument_variable(arguments: &[Node<ast::Argument>], name: &Name) -> Option<Name> {
    match arguments
        .iter()
        .find(|argument| argument.name == *name)?
        .value
        .as_ref()
    {
        ast::Value::Variable(variable) => Some(variable.clone()),
        _ => None,
    }
}

/// Every contextual use a fetch's operation makes, read from `root_type` in the subgraph's own
/// schema (`Subgraph.fetchContextUsesAll`): a field selected at a runtime type declaring a
/// `@fromContext` argument there.
///
/// The walk carries a pool of runtime types, as [`super::resolvable`]'s does, each with the types
/// its ancestors can have (`ContextWalkState`), and reads an inline fragment only at the types it
/// applies to, so each selection is visited once per pool. The paths reaching one type share its
/// state, their ancestors merged level by level (`mergeAncestors`), which is exact for the
/// nearest-setter test, asking the same of every type at a level.
pub(super) fn context_uses(
    subgraph: &Subgraph<'_>,
    subgraph_schema: &ValidFederationSchema,
    root_type: &Name,
    selections: &[Selection],
) -> Result<Vec<ContextUse>, ComparisonError> {
    let mut uses = Vec::new();
    collect_context_uses(
        subgraph,
        subgraph_schema,
        &[(root_type.clone(), Vec::new())],
        selections,
        &mut uses,
    )?;
    Ok(uses)
}

/// A runtime type the walk over an operation reaches, with the types each of its ancestors can
/// have, one list per level from the walk's start. The paths reaching one type through one object
/// the fetch runs on share a state ([`add_context_walk_state`]).
type ContextWalkState = (Name, Vec<Vec<Name>>);

/// Adds a state to a pool: into the state for the same type reached through the same ancestor
/// below the start, the object an entity fetch runs on, the ancestors merged level by level as
/// `List.zipWith` pairs them, or as a new one. That ancestor so stays one type.
fn add_context_walk_state(pool: &mut Vec<ContextWalkState>, state: ContextWalkState) {
    let Some((_, ancestors)) = pool.iter_mut().find(|(object_type, ancestors)| {
        *object_type == state.0 && ancestors.get(1) == state.1.get(1)
    }) else {
        pool.push(state);
        return;
    };
    let merged = ancestors
        .iter()
        .zip(state.1)
        .map(|(left, right)| {
            let mut level = left.clone();
            for object_type in right {
                if !level.contains(&object_type) {
                    level.push(object_type);
                }
            }
            level
        })
        .collect();
    *ancestors = merged;
}

fn collect_context_uses(
    subgraph: &Subgraph<'_>,
    schema: &ValidFederationSchema,
    states: &[ContextWalkState],
    selections: &[Selection],
    uses: &mut Vec<ContextUse>,
) -> Result<(), ComparisonError> {
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                let mut below: Vec<ContextWalkState> = Vec::new();
                for (object_type, ancestors) in states {
                    for declaration in subgraph.context_arguments(object_type, &field.name)? {
                        let found = ContextUse {
                            parent_type: object_type.clone(),
                            field_name: field.name.clone(),
                            variable: argument_variable(
                                &field.arguments,
                                &declaration.argument_name,
                            ),
                            declaration,
                            levels: ancestors
                                .iter()
                                .cloned()
                                .chain(std::iter::once(vec![object_type.clone()]))
                                .collect(),
                        };
                        if !uses.contains(&found) {
                            uses.push(found);
                        }
                    }
                    if !field.selection_set.selections.is_empty()
                        && let Some(child_type) =
                            field_output_type(schema, object_type, &field.name)
                    {
                        for runtime_type in possible_types(schema, &child_type) {
                            let mut child_ancestors = ancestors.clone();
                            child_ancestors.push(vec![object_type.clone()]);
                            add_context_walk_state(&mut below, (runtime_type, child_ancestors));
                        }
                    }
                }
                if !below.is_empty() {
                    collect_context_uses(
                        subgraph,
                        schema,
                        &below,
                        &field.selection_set.selections,
                        uses,
                    )?;
                }
            }
            Selection::InlineFragment(fragment) => {
                let applying: Vec<ContextWalkState> = states
                    .iter()
                    .filter(|(object_type, _)| {
                        does_fragment_type_apply(
                            schema,
                            object_type,
                            fragment.type_condition.as_ref(),
                        )
                    })
                    .cloned()
                    .collect();
                if !applying.is_empty() {
                    collect_context_uses(
                        subgraph,
                        schema,
                        &applying,
                        &fragment.selection_set.selections,
                        uses,
                    )?;
                }
            }
            // Spreads are inlined before a fetch's operation is read.
            Selection::FragmentSpread(_) => {}
        }
    }
    Ok(())
}

/// A fetch with no `requires` passes no contextual argument (`RootFetchesUseNoContext`): the router
/// runs context rewrites only for a fetch it sends representations (`Variables::new`), so such an
/// argument reaches the subgraph with no value.
pub(super) fn check_root_fetch_uses_no_context(
    fetch: &FetchNode,
    uses: &[ContextUse],
) -> Result<(), ComparisonError> {
    match uses.first() {
        None => Ok(()),
        Some(found) => Err(ComparisonError::new(format!(
            "fetch to {}: it passes {}, but a fetch with no `requires` runs no context rewrite to \
             bind it",
            fetch.subgraph_name,
            found.describe()
        ))),
    }
}

/// Every `@fromContext` argument a fetch's operation selects is passed a variable
/// (`FetchContextArgumentsPassVariables`). An omitted argument or a literal fails this. Which
/// rewrites must bind the variable is [`check_context_declared`]'s question: where none does, the
/// subgraph gets no value, which is right where the declaration has none.
pub(super) fn check_context_arguments_pass_variables(
    fetch: &FetchNode,
    uses: &[ContextUse],
) -> Result<(), ComparisonError> {
    match uses.iter().find(|found| found.variable.is_none()) {
        None => Ok(()),
        Some(found) => Err(ComparisonError::new(format!(
            "fetch to {}: {} is passed no variable",
            fetch.subgraph_name,
            found.describe()
        ))),
    }
}

/// One selection of a `@fromContext` declaration, read syntactically, its arguments left out: the
/// model's `FieldSet`, which a declaration's selection is.
enum DeclaredSelection {
    Field(Name, Vec<DeclaredSelection>),
    InlineFragment(Option<Name>, Vec<DeclaredSelection>),
}

/// Reads a declaration's selection, written with or without its braces, as `FieldSet::parse`
/// accepts it.
fn parse_declared_selection(selection: &str) -> Result<Vec<DeclaredSelection>, String> {
    let source = if selection.trim_start().starts_with('{') {
        selection.to_string()
    } else {
        format!("{{ {selection} }}")
    };
    let document =
        ast::Document::parse(source, "fromContext.graphql").map_err(|e| e.errors.to_string())?;
    let Some(ast::Definition::OperationDefinition(operation)) = document.definitions.first() else {
        return Err("not a selection".into());
    };
    fn convert(selections: &[ast::Selection]) -> Vec<DeclaredSelection> {
        selections
            .iter()
            .filter_map(|selection| match selection {
                ast::Selection::Field(field) => Some(DeclaredSelection::Field(
                    field.name.clone(),
                    convert(&field.selection_set),
                )),
                ast::Selection::InlineFragment(fragment) => {
                    Some(DeclaredSelection::InlineFragment(
                        fragment.type_condition.clone(),
                        convert(&fragment.selection_set),
                    ))
                }
                // A field set has no fragment spreads.
                ast::Selection::FragmentSpread(_) => None,
            })
            .collect()
    }
    Ok(convert(&operation.selection_set))
}

/// A field of a type, or the `__typename` every object, interface and union has though none
/// declares it, which is all a context declared on a union can select directly: its named type.
fn field_or_typename_type(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    field_name: &Name,
) -> Option<Name> {
    field_output_type(schema, parent_type, field_name).or_else(|| {
        (*field_name == TYPENAME_FIELD
            && matches!(
                schema.schema().types.get(parent_type),
                Some(ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_))
            ))
        .then(|| name!("String"))
    })
}

/// The fields of a name a declaration selects at one level, for data of the given runtime types,
/// each with its own type and what it selects below: directly, or under an inline fragment applying
/// to one of the types, which is where the static parent changes (`declaredFieldsNamed`).
fn declared_fields_named<'d>(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    types: &[Name],
    field_name: &Name,
    selections: &'d [DeclaredSelection],
    out: &mut Vec<(Name, &'d [DeclaredSelection])>,
) {
    for selection in selections {
        match selection {
            DeclaredSelection::Field(name, below) => {
                if name == field_name
                    && let Some(field_type) = field_or_typename_type(schema, parent_type, name)
                {
                    out.push((field_type, below));
                }
            }
            DeclaredSelection::InlineFragment(type_condition, below) => {
                let narrowed: Vec<Name> = types
                    .iter()
                    .filter(|object_type| {
                        does_fragment_type_apply(schema, object_type, type_condition.as_ref())
                    })
                    .cloned()
                    .collect();
                if !narrowed.is_empty() {
                    declared_fields_named(
                        schema,
                        type_condition.as_ref().unwrap_or(parent_type),
                        &narrowed,
                        field_name,
                        below,
                        out,
                    );
                }
            }
        }
    }
}

/// Do the fields a copy's keys stand for below the object a rewrite climbed to read a
/// declaration's selection down to a leaf (`ReadsDeclaredLeaf`)? Each is a field of that name the
/// declaration selects at its level, for the runtime types the copy has there, and the last selects
/// nothing below. `levels` are the copy's levels below the first field, one per field but the last.
fn reads_declared_leaf(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    types: &[Name],
    selections: &[DeclaredSelection],
    field_path: &[Name],
    levels: &[&LevelTypes],
) -> bool {
    let Some((field_name, rest)) = field_path.split_first() else {
        return false;
    };
    let mut fields = Vec::new();
    declared_fields_named(
        schema,
        parent_type,
        types,
        field_name,
        selections,
        &mut fields,
    );
    fields.into_iter().any(|(field_type, below)| {
        if rest.is_empty() {
            return below.is_empty();
        }
        let Some((level, level_rest)) = levels.split_first() else {
            return false;
        };
        reads_declared_leaf(schema, &field_type, level, below, rest, level_rest)
    })
}

/// How many levels a rewrite climbs: its leading `Parent`s.
fn context_climb(path: &[FetchDataPathElement]) -> usize {
    path.iter()
        .take_while(|element| matches!(element, FetchDataPathElement::Parent))
        .count()
}

/// How many levels below the query root a fetch runs: one per response key of its path.
fn fetch_path_depth(path: &[FetchDataPathElement]) -> usize {
    path.iter()
        .filter(|element| matches!(element, FetchDataPathElement::Key(_, _)))
        .count()
}

/// Everything [`check_context_declared`] reads of the fetch and where it runs.
pub(super) struct ContextSite<'a> {
    pub(super) schema: &'a ValidFederationSchema,
    pub(super) root_type: &'a Name,
    pub(super) path: &'a [FetchDataPathElement],
    pub(super) available: &'a [Selection],
    pub(super) readable: &'a [Selection],
    pub(super) fetch: &'a FetchNode,
    pub(super) requires_types: &'a [Name],
}

/// Every contextual value an entity fetch passes comes from where the argument's declaration says,
/// wherever it has one (`FetchSiteContextDeclared`): each rewrite binding the variable a contextual
/// argument is passed reads the declared leaf off an object setting the context, and every value
/// the declaration has is one a rewrite targets ([`check_owed_values_targeted`]). Each such rewrite
/// is held to it, availability ([`check_context_rewrites`]) being a disjunction over them and this
/// not.
///
/// A rewrite is a path the planner writes from the declaration
/// (`add_context_renamers_for_selection_set`), and the router reads it key by key in the response,
/// where a key holds whatever an earlier fetch put under that response key; nothing at run time
/// compares what it reads with the declaration. So it is read back against the declaration through
/// what the fetch can read: in every copy an execution of the fetch reads, the object the rewrite
/// climbs to has a type that sets the context and is the nearest ancestor of the field's object
/// that can, and the fields the copy's keys stand for below it read down to a leaf of the
/// declaration's selection. Every rewrite binding the variable is held to this, though the router
/// takes the value from the first that finds one.
pub(super) fn check_context_declared(
    site: &ContextSite<'_>,
    subgraph: &Subgraph<'_>,
    uses: &[ContextUse],
) -> Result<(), ComparisonError> {
    for found in uses {
        let Some(variable) = &found.variable else {
            continue;
        };
        for rewrite in site.fetch.context_rewrites.iter() {
            let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
                continue;
            };
            if renamer.rename_key_to != *variable {
                continue;
            }
            check_rewrite_matches_declaration(site, subgraph, found, &renamer.path).map_err(
                |reason| {
                    ComparisonError::new(format!(
                        "fetch to {}: the rewrite binding `${variable}` for {} {reason}\n* fetch \
                     runs at: {}\n* rewrite reads: {}",
                        site.fetch.subgraph_name,
                        found.describe(),
                        display_path(site.path),
                        display_path(&renamer.path),
                    ))
                },
            )?;
        }
        check_owed_values_targeted(site, subgraph, found, variable).map_err(|reason| {
            ComparisonError::new(format!(
                "fetch to {}: {} is passed `${variable}`, {reason}\n* fetch runs at: {}",
                site.fetch.subgraph_name,
                found.describe(),
                display_path(site.path),
            ))
        })?;
    }
    Ok(())
}

/// Does a fetch's operation declare a variable non-null (`FetchNode.declaresVariableNonNull`)? An
/// unbound one fails the fetch.
fn declares_variable_non_null(fetch: &FetchNode, variable_name: &Name) -> bool {
    fetch
        .operation_document
        .as_parsed()
        .ok()
        .and_then(|document| document.operations.get(None).ok())
        .is_some_and(|operation| {
            operation
                .variables
                .iter()
                .any(|definition| definition.name == *variable_name && definition.ty.is_non_null())
        })
}

/// Does a declaration's selection select something at an object of type `object_type`
/// (`FieldSet.selectsAnythingAt`)? A field does, and a fragment does where it applies and selects
/// something itself.
fn selects_anything_at(
    schema: &ValidFederationSchema,
    object_type: &Name,
    selections: &[DeclaredSelection],
) -> bool {
    selections.iter().any(|selection| match selection {
        DeclaredSelection::Field(_, _) => true,
        DeclaredSelection::InlineFragment(type_condition, below) => {
            does_fragment_type_apply(schema, object_type, type_condition.as_ref())
                && selects_anything_at(schema, object_type, below)
        }
    })
}

/// Does an object of type `object_type` set the context: does one of `setters`, the types the
/// subgraph sets it on, include it (`ContextRead.setsContext`)?
fn sets_context(schema: &ValidFederationSchema, setters: &[Name], object_type: &Name) -> bool {
    setters
        .iter()
        .any(|setter| type_includes_object(schema, setter, object_type))
}

/// Every value a contextual use is owed is one a rewrite binding its variable targets
/// (`OwedContextValuesTargeted`). A value is owed where the fetch declares the variable non-null,
/// an unbound one failing the fetch, and else where the declaration's selection selects something
/// at the type of the nearest ancestor setting the context ([`selects_anything_at`]):
/// - at every position of the fetch and runtime configuration of its levels where the nearest
///   setter among the ancestors already fetched owes one, a rewrite targets the configuration
///   ([`context_variable_targeted`]); the object the fetch runs on counts where the field is
///   selected below it;
/// - no setter among the ancestors inside the fetch's operation, past the object it runs on, owes
///   one, as no rewrite can read it.
///
/// `Err` says why not.
fn check_owed_values_targeted(
    site: &ContextSite<'_>,
    subgraph: &Subgraph<'_>,
    found: &ContextUse,
    variable: &Name,
) -> Result<(), String> {
    let setters = subgraph.context_types(&found.declaration.context);
    let non_null = declares_variable_non_null(site.fetch, variable);
    let mut declared = None;
    let mut owes_value = |setter_type: Option<&Name>| -> Result<bool, String> {
        if non_null {
            return Ok(true);
        }
        let Some(setter_type) = setter_type else {
            return Ok(false);
        };
        let selection = match &declared {
            Some(selection) => selection,
            None => declared.insert(
                parse_declared_selection(&found.declaration.selection).map_err(|e| {
                    format!(
                        "whose declaration's selection `{}` does not parse: {e}",
                        found.declaration.selection
                    )
                })?,
            ),
        };
        Ok(selects_anything_at(site.schema, setter_type, selection))
    };
    for position in fetch_positions(
        site.schema,
        site.root_type,
        site.available,
        site.path,
        site.requires_types,
    ) {
        // Only where the use is made, or everywhere for a non-null variable, an unbound one failing
        // the fetch whatever its object.
        let at_types = position.current.iter().filter(|entity_type| {
            non_null || context_use_made_at(site.schema, &found.levels, entity_type)
        });
        for at_type in at_types.map(|entity_type| position.at_type(entity_type)) {
            let levels = at_type.type_levels();
            for runtime_types in runtime_combinations(levels.len(), &levels) {
                // `ContextRead.fetchedAncestorTypes`: the field's own object is no ancestor of it.
                let fetched = if found.levels.len() <= 2 {
                    &runtime_types[..runtime_types.len().saturating_sub(1)]
                } else {
                    &runtime_types[..]
                };
                let nearest = fetched
                    .iter()
                    .rev()
                    .find(|object_type| sets_context(site.schema, &setters, object_type));
                if owes_value(nearest)?
                    && !context_variable_targeted(
                        site.schema,
                        site.path,
                        &site.fetch.context_rewrites,
                        &runtime_types,
                        variable,
                    )
                {
                    let owed = match nearest {
                        _ if non_null => "declared non-null".to_string(),
                        Some(setter_type) => format!("owed a value off a `{setter_type}`"),
                        None => unreachable!("owed a value with no setter only when non-null"),
                    };
                    return Err(format!(
                        "{owed} where no rewrite binding it reads one\n* for data of: {}",
                        runtime_types
                            .iter()
                            .map(|type_name| type_name.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ));
                }
            }
        }
    }
    // `ContextRead.unfetchedAncestors`: past the operation's root and the object the fetch runs on,
    // the field's own object aside.
    let inside = found
        .levels
        .get(2..found.levels.len().saturating_sub(1))
        .unwrap_or_default();
    for object_type in inside.iter().flatten() {
        if sets_context(site.schema, &setters, object_type) && owes_value(Some(object_type))? {
            return Err(format!(
                "owed a value off a `{object_type}` inside the fetch's operation, which no \
                 rewrite can read"
            ));
        }
    }
    Ok(())
}

/// A context rewrite reads what a declaration asks for (`ContextRewriteMatchesDeclaration`). At
/// every position of the fetch, for every runtime configuration and every copy the rewrite reads
/// there, the object the rewrite climbs to has a type that sets the context, and the copy is the
/// declared value of the nearest setter (`ContextRead.FromNearestSetter`): that object is an
/// ancestor of the field's object, the field's own object not counting, no ancestor between can
/// have a type setting the context, and the copy's fields below it read down to a leaf of that
/// type's selection. The ancestors run through the position, then the fetch's operation
/// (`contextFieldAncestors`). Each copy is read at its own types and fields. A
/// rewrite that reads no copy passes; that is availability's concern. `Err` says why not.
fn check_rewrite_matches_declaration(
    site: &ContextSite<'_>,
    subgraph: &Subgraph<'_>,
    found: &ContextUse,
    context_path: &[FetchDataPathElement],
) -> Result<(), String> {
    let Some(merged) = merge_context_path(site.path, context_path) else {
        return Ok(());
    };
    let depth = fetch_path_depth(site.path).saturating_sub(context_climb(context_path));
    let shared = shared_levels(&merged, site.path);
    let copies = path_copies(site.schema, site.root_type, site.readable, &merged);
    let setters = subgraph.context_types(&found.declaration.context);
    let mut declared = None;
    let positions = fetch_positions(
        site.schema,
        site.root_type,
        site.available,
        site.path,
        site.requires_types,
    );
    // Only at the objects the fetch runs on where the use is made: elsewhere the value the rewrite
    // reads goes unused.
    let made_at = positions.iter().flat_map(|position| {
        position
            .current
            .iter()
            .filter(|entity_type| context_use_made_at(site.schema, &found.levels, entity_type))
            .map(|entity_type| position.at_type(entity_type))
    });
    for position in made_at {
        for runtime_types in runtime_combinations(shared, &position.type_levels()) {
            let Some(object_type) = runtime_types.get(depth) else {
                continue;
            };
            for copy in context_read_copies(&copies, site.path, &merged, &runtime_types) {
                let Some(setter) = setters
                    .iter()
                    .find(|setter| type_includes_object(site.schema, setter, object_type))
                else {
                    return Err(format!(
                        "climbs to a `{object_type}`, which sets no context `{}`",
                        found.declaration.context
                    ));
                };
                let ancestors = context_field_ancestors(&position, &found.levels);
                if depth >= ancestors.len() {
                    return Err(format!(
                        "climbs to the `{object_type}` the field is selected on, where a value \
                         comes from an object above it"
                    ));
                }
                if let Some((nearer, nearer_setter)) =
                    setter_among(site.schema, &setters, &ancestors[depth + 1..])
                {
                    return Err(format!(
                        "climbs past a `{nearer}` nearer the field, on which `{nearer_setter}` \
                         sets context `{}`",
                        found.declaration.context
                    ));
                }
                let selection = match &declared {
                    Some(selection) => selection,
                    None => declared.insert(
                        parse_declared_selection(&found.declaration.selection).map_err(|e| {
                            format!(
                                "has a declaration whose selection `{}` does not parse: {e}",
                                found.declaration.selection
                            )
                        })?,
                    ),
                };
                let levels = copy.type_levels();
                if !reads_declared_leaf(
                    site.schema,
                    setter,
                    std::slice::from_ref(object_type),
                    selection,
                    copy.field_path.get(depth..).unwrap_or_default(),
                    levels.get(depth + 1..).unwrap_or_default(),
                ) {
                    return Err(format!(
                        "reads `{}` off a `{object_type}`, which is not a leaf of the declared \
                         `{}`",
                        copy.field_path
                            .get(depth..)
                            .unwrap_or_default()
                            .iter()
                            .map(|name| name.as_str())
                            .collect::<Vec<_>>()
                            .join("."),
                        found.declaration.selection.trim()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The types each ancestor of a contextual field's object can have in the response, outermost
/// first, one list per level, for a fetch at `position` (`contextFieldAncestors`): the position's
/// levels down to the object the fetch runs on, as it admits them, then the levels below that
/// object in `use_levels`, the types of the field's object and its ancestors in the fetch's
/// operation, less the field's object itself.
fn context_field_ancestors(position: &ReachedCopy, use_levels: &[Vec<Name>]) -> Vec<Vec<Name>> {
    let mut ancestors: Vec<Vec<Name>> = position
        .type_levels()
        .into_iter()
        .cloned()
        .chain(use_levels.iter().skip(2).cloned())
        .collect();
    ancestors.pop();
    ancestors
}

/// A type an object at `levels` can have that one of `setters` includes, with that setter: `None`
/// is the model's `ContextRead.NoSetterAmong`.
fn setter_among<'s>(
    schema: &ValidFederationSchema,
    setters: &'s [Name],
    levels: &[Vec<Name>],
) -> Option<(Name, &'s Name)> {
    levels.iter().flatten().find_map(|object_type| {
        setters
            .iter()
            .find(|setter| type_includes_object(schema, setter, object_type))
            .map(|setter| (object_type.clone(), setter))
    })
}

/// A path as the plan prints it.
pub(super) fn display_path(path: &[FetchDataPathElement]) -> String {
    path.iter()
        .map(|element| element.to_string())
        .collect::<Vec<_>>()
        .join(".")
}
