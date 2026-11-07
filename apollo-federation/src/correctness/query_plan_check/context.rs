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
//! admit the type the data has at every shared level and are unnarrowed past them, nothing about
//! the fetch saying what those deeper objects are; the fetch's own narrowing past the shared
//! levels is ignored, restricting when it runs and not where the value is. What is left is
//! Boolean, and [`boolean_condition_covered_by`] decides that symbolically.
//!
//! Past the shared levels a value is often split into copies, each narrowed to some of the types a
//! level allows: a fetch writes the split with a fragment per type, and one fetch per type makes it
//! with path conditions. Copies alike but at one level, under the same guards, are siblings, and
//! each is read with that level widened to every type its siblings admit
//! ([`ReachedCopy::widened_at`]), so between them they serve what one unnarrowed copy would.
//! Copies narrowed apart at two levels, or guarded differently, are still read one at a time: a
//! conservative limit.
//!
//! A level carries both the types the schema allows there and what the guards narrow that to,
//! which is what tells a real narrowing from a vacuous one: a `... on T` where `T` is the only
//! type the position can have narrows nothing, and reading the guard names alone would reject
//! every unnarrowed fetch position against it.
//!
//! One variable at a time, not one rewrite at a time. A fetch can carry several rewrites binding
//! the same variable, one per runtime type the context object may have, and the router takes the
//! value from whichever resolves — `execute_on_path` claims the variable only once a rewrite has
//! produced a value, so one that finds nothing falls through to the next.
//! [`context_value_clauses`] gathers the copies of every rewrite binding the variable before the
//! coverage test, and a rewrite whose path does not resolve contributes nothing rather than
//! failing on its own.
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
//! # Soundness: the argument has to be bound, and to what it declares
//!
//! A rewrite that reaches nothing is not the only way an argument goes unbound. The router runs
//! context rewrites only for a fetch it sends representations, so a root fetch passing a
//! `@fromContext` argument passes it nothing ([`check_root_fetch_uses_no_context`]); and a fetch
//! that drops both the argument and its rewrite leaves no rewrite for availability to check
//! ([`check_context_arguments_bound`]). Both are read off the fetch's operation in its
//! subgraph's schema ([`context_uses`]), which says which arguments are contextual.
//!
//! Nor does anything at run time compare what a rewrite reads with the declaration: a key holds
//! whatever an earlier fetch put under it. So every rewrite binding a contextual argument is read
//! back against the declaration ([`check_context_declared`]): the object it climbs to must have a
//! type setting the context, and the fields its keys stand for below it must read down to a leaf
//! of the declared selection, by field name rather than response key.

#[cfg(test)]
#[path = "context_test.rs"]
mod context_test;

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::conditions::BooleanCondition;
use super::super::query_compare::conditions::BooleanLiteral;
use super::super::query_compare::conditions::boolean_condition_covered_by;
use super::super::query_compare::conditions::canonical_boolean_condition;
use super::super::query_compare::conditions::literals_for_directives;
use super::ComparisonError;
use super::selections::TypeCondition;
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

/// The object types one level of a path can have: what the schema allows an object there to be,
/// and what the fragments and `TypenameEquals` in force narrow that to.
#[derive(Clone, PartialEq)]
struct LevelTypes {
    allowed: Vec<Name>,
    narrowed: Vec<Name>,
}

impl LevelTypes {
    /// The level a path starts at, and the one a field's selection set starts at: every type the
    /// position can have, nothing narrowing it yet.
    fn of(schema: &ValidFederationSchema, parent_type: &Name) -> Self {
        let possible = possible_types(schema, parent_type);
        LevelTypes {
            narrowed: possible.clone(),
            allowed: possible,
        }
    }

    /// Narrows to one type condition of a fragment or a `TypenameEquals`.
    fn narrow_by(&self, schema: &ValidFederationSchema, type_name: &Name) -> Self {
        self.narrowed_to(&possible_types(schema, type_name))
    }

    /// Narrows by a path element's own type condition, a disjunction of object names compared
    /// literally with each object's `__typename`, as the router's `iterate_path` compares them: the
    /// level keeps the types it holds that the condition names. A name that is no object type, an
    /// interface's or a union's, keeps nothing, so a value read below it is read nowhere.
    fn narrow_by_condition(&self, type_condition: Option<&TypeCondition>) -> Self {
        match type_condition {
            None => self.clone(),
            Some(type_names) => self.narrowed_to(type_names),
        }
    }

    fn narrowed_to(&self, admitted: &[Name]) -> Self {
        LevelTypes {
            allowed: self.allowed.clone(),
            narrowed: self
                .narrowed
                .iter()
                .filter(|type_name| admitted.contains(type_name))
                .cloned()
                .collect(),
        }
    }

    /// Does nothing narrow this level — is every type the schema allows here one the guards in
    /// force admit?
    fn unnarrowed(&self) -> bool {
        self.allowed
            .iter()
            .all(|type_name| self.narrowed.contains(type_name))
    }
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

    /// Is `other` a sibling of this copy at one level: the same guards, and the same types at
    /// every level but that one, where the two may be narrowed apart?
    fn sibling_at(&self, level: usize, other: &ReachedCopy) -> bool {
        let (mine, theirs) = (self.type_levels(), other.type_levels());
        self.condition == other.condition
            && mine.len() == theirs.len()
            && mine.iter().zip(&theirs).enumerate().all(|(index, (a, b))| {
                if index == level {
                    a.allowed == b.allowed
                } else {
                    a == b
                }
            })
    }

    /// This copy's levels, the one at `level` widened to every type its siblings there admit
    /// ([`Self::sibling_at`]): copies narrowed apart at one level, and alike elsewhere, serve
    /// together what one copy admitting all their types would.
    fn widened_at(&self, copies: &[ReachedCopy], level: usize) -> Vec<LevelTypes> {
        let narrowed = copies
            .iter()
            .filter(|other| self.sibling_at(level, other))
            .flat_map(|sibling| {
                sibling
                    .type_levels()
                    .get(level)
                    .map(|types| types.narrowed.clone())
                    .unwrap_or_default()
            })
            .collect();
        let mut levels: Vec<LevelTypes> = self.type_levels().into_iter().cloned().collect();
        if let Some(types) = levels.get_mut(level) {
            types.narrowed = narrowed;
        }
        levels
    }
}

/// What a path addresses: the value its last key holds, which a rewrite reads, or the object that
/// value is, which a fetch runs on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PathEnd {
    Value,
    Object,
}

/// The copy a path starts from: the query root, unnarrowed and unguarded.
fn root_reached(schema: &ValidFederationSchema, root_type: &Name) -> ReachedCopy {
    ReachedCopy {
        levels: Vec::new(),
        current: LevelTypes::of(schema, root_type),
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
        position.current = position.current.narrowed_to(requires_types);
        (!position.current.narrowed.is_empty()).then_some(position)
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
        if reached.current.narrowed.is_empty() {
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
            &narrowed(reached.current.narrow_by(schema, type_name)),
            ending,
            rest,
            selections,
        ),
        FetchDataPathElement::AnyIndex(type_condition) => copies_reached(
            schema,
            parent_type,
            &narrowed(
                reached
                    .current
                    .narrow_by_condition(type_condition.as_deref()),
            ),
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
                if field.response_name() != name || reached.current.narrowed.is_empty() {
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
                let level = LevelTypes::of(schema, &child_type).narrow_by_condition(type_condition);
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
                    Some(fragment_type) => reached.current.narrow_by(schema, fragment_type),
                    None => reached.current.clone(),
                };
                if narrowed.narrowed.is_empty() {
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

/// The named type a field returns, as the supergraph declares it.
fn field_output_type(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    field_name: &Name,
) -> Option<Name> {
    let field = match schema.schema().types.get(parent_type)? {
        ExtendedType::Object(ty) => ty.fields.get(field_name),
        ExtendedType::Interface(ty) => ty.fields.get(field_name),
        _ => None,
    }?;
    Some(field.ty.inner_named_type().clone())
}

/// Do a copy's levels admit one runtime configuration?
///
/// At every level the two paths share they must admit the type the data has there, and past those
/// levels they must be unnarrowed, nothing about the fetch saying what those objects are.
fn type_levels_admit(value: &[&LevelTypes], runtime_types: &[Name]) -> bool {
    let Some((level, rest)) = value.split_first() else {
        return true;
    };
    match runtime_types.split_first() {
        None => level.unnarrowed() && type_levels_admit(rest, &[]),
        Some((type_name, runtime_rest)) => {
            level.narrowed.contains(type_name) && type_levels_admit(rest, runtime_rest)
        }
    }
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
            level.narrowed.iter().map(move |type_name| {
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

/// The clauses under which a fetch can read the value bound to one context variable, for data of
/// one runtime configuration: the conditions of the copies each rewrite binding the variable reads
/// there ([`context_read_copies`]).
fn context_value_clauses(
    schema: &ValidFederationSchema,
    root_type: &Name,
    path: &[FetchDataPathElement],
    available: &[Selection],
    context_rewrites: &[Arc<FetchDataRewrite>],
    runtime_types: &[Name],
    variable_name: &Name,
) -> Vec<BooleanCondition> {
    let mut clauses = Vec::new();
    for rewrite in context_rewrites {
        let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
            continue;
        };
        if renamer.rename_key_to != *variable_name {
            continue;
        }
        let Some(merged) = merge_context_path(path, &renamer.path) else {
            continue;
        };
        let copies = path_copies(schema, root_type, available, &merged);
        clauses.extend(
            context_read_copies(&copies, path, &merged, runtime_types)
                .map(|copy| copy.condition.clone()),
        );
    }
    clauses
}

/// The copies of a context value one runtime configuration of the fetch's position reads: those
/// whose levels admit it over the levels the merged path shares with the fetch's, alone or widened
/// at one level past them by their siblings there ([`ReachedCopy::widened_at`]). `copies` is
/// [`path_copies`] of `merged`.
fn context_read_copies<'c>(
    copies: &'c [ReachedCopy],
    path: &[FetchDataPathElement],
    merged: &[FetchDataPathElement],
    runtime_types: &[Name],
) -> impl Iterator<Item = &'c ReachedCopy> {
    let shared = shared_levels(merged, path);
    let configuration = runtime_types[..runtime_types.len().min(shared)].to_vec();
    copies.iter().filter(move |copy| {
        let levels = copy.type_levels();
        type_levels_admit(&levels, &configuration)
            || (configuration.len()..levels.len()).any(|level| {
                let widened = copy.widened_at(copies, level);
                type_levels_admit(&widened.iter().collect::<Vec<_>>(), &configuration)
            })
    })
}

/// Is every contextual value a fetch reads already fetched wherever the fetch runs?
///
/// Deciding this per copy of the fetch's own position is what makes the shared prefix cancel; see
/// the module docs.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_context_rewrites(
    schema: &ValidFederationSchema,
    root_type: &Name,
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
            for runtime_types in runtime_combinations(shared, &position.type_levels()) {
                let covers = context_value_clauses(
                    schema,
                    root_type,
                    path,
                    readable,
                    &fetch.context_rewrites,
                    &runtime_types,
                    variable,
                );
                let mut running = condition.to_vec();
                running.extend(position.condition.iter().cloned());
                // A position the fetch can never run at is met by nothing at all: asking for it
                // to be covered would reject a plan over data that does not exist.
                let Some(reachable) = canonical_boolean_condition(&running) else {
                    continue;
                };
                if !boolean_condition_covered_by(&reachable, &covers) {
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
    Ok(())
}

//==================================================================================================
// Contextual arguments: bound by an entity fetch, passed by no other
//==================================================================================================

/// A `@fromContext` argument a fetch's operation passes: the field it is an argument of, its
/// declaration, and the variable the operation passes it, if it passes one (the model's
/// `ContextUse`).
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ContextUse {
    parent_type: Name,
    field_name: Name,
    declaration: ContextArgument,
    variable: Option<Name>,
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
/// `@fromContext` argument there. The walk carries a pool of runtime types, as
/// [`super::resolvable`]'s does, and reads an inline fragment only at the types it applies to, so
/// each selection is visited once per pool.
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
        std::slice::from_ref(root_type),
        selections,
        &mut uses,
    )?;
    Ok(uses)
}

fn collect_context_uses(
    subgraph: &Subgraph<'_>,
    schema: &ValidFederationSchema,
    types: &[Name],
    selections: &[Selection],
    uses: &mut Vec<ContextUse>,
) -> Result<(), ComparisonError> {
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                let mut below: IndexSet<Name> = IndexSet::default();
                for object_type in types {
                    for declaration in subgraph.context_arguments(object_type, &field.name)? {
                        let found = ContextUse {
                            parent_type: object_type.clone(),
                            field_name: field.name.clone(),
                            variable: argument_variable(
                                &field.arguments,
                                &declaration.argument_name,
                            ),
                            declaration,
                        };
                        if !uses.contains(&found) {
                            uses.push(found);
                        }
                    }
                    if let Some(child_type) = field_output_type(schema, object_type, &field.name) {
                        below.extend(possible_types(schema, &child_type));
                    }
                }
                if !field.selection_set.selections.is_empty() {
                    let below: Vec<Name> = below.into_iter().collect();
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
                let applying: Vec<Name> = types
                    .iter()
                    .filter(|object_type| {
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

/// Spec `DoesFragmentTypeApply` at a runtime object type.
fn does_fragment_type_apply(
    schema: &ValidFederationSchema,
    object_type: &Name,
    type_condition: Option<&Name>,
) -> bool {
    type_condition.is_none_or(|condition| possible_types(schema, condition).contains(object_type))
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

/// Every `@fromContext` argument a fetch's operation selects is passed a variable that one of the
/// fetch's context key renamers binds (`FetchContextArgumentsBound`). An omitted argument, a
/// literal, or a variable no renamer binds fails this.
pub(super) fn check_context_arguments_bound(
    fetch: &FetchNode,
    uses: &[ContextUse],
) -> Result<(), ComparisonError> {
    let bound = context_variables(&fetch.context_rewrites);
    for found in uses {
        let reason = match &found.variable {
            None => "is passed no variable".to_string(),
            Some(variable) if !bound.contains(variable) => {
                format!("is passed `${variable}`, which no context rewrite binds")
            }
            Some(_) => continue,
        };
        return Err(ComparisonError::new(format!(
            "fetch to {}: {} {reason}",
            fetch.subgraph_name,
            found.describe()
        )));
    }
    Ok(())
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
        reads_declared_leaf(
            schema,
            &field_type,
            &level.narrowed,
            below,
            rest,
            level_rest,
        )
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

/// Every contextual value an entity fetch passes comes from where the argument's declaration says
/// (`FetchSiteContextDeclared`): each rewrite binding the variable a contextual argument is passed
/// reads the declared leaf off an object setting the context. Each such rewrite is held to it,
/// availability ([`check_context_rewrites`]) being a disjunction over them and this not.
///
/// A rewrite is a path the planner writes from the declaration
/// (`add_context_renamers_for_selection_set`), and the router reads it key by key in the response,
/// where a key holds whatever an earlier fetch put under that response key; nothing at run time
/// compares what it reads with the declaration. So it is read back against the declaration through
/// what the fetch can read: in every copy an execution of the fetch reads, the object the rewrite
/// climbs to has a type that sets the context, and the fields the copy's keys stand for below it
/// read down to a leaf of the declaration's selection. Which setter is the nearest is not asked.
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
    }
    Ok(())
}

/// A context rewrite reads what a declaration asks for (`ContextRewriteMatchesDeclaration`). At
/// every position of the fetch, for every runtime configuration and every copy the rewrite reads
/// there, the object the rewrite climbs to has a type that sets the context, and the copy's fields
/// below it read down to a leaf of that type's selection. Each copy is read at its own types and
/// fields, siblings included. A rewrite that reads no copy passes; that is availability's concern.
/// `Err` says why not.
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
    for position in fetch_positions(
        site.schema,
        site.root_type,
        site.available,
        site.path,
        site.requires_types,
    ) {
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

/// A path as the plan prints it.
pub(super) fn display_path(path: &[FetchDataPathElement]) -> String {
    path.iter()
        .map(|element| element.to_string())
        .collect::<Vec<_>>()
        .join(".")
}
