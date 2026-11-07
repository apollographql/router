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
//! [`path_conditions`] follows a path and returns one entry per copy of what it addresses, holding
//! what guards that copy. The obligation compares two of those — the value's, and the fetch's own
//! position. The context object is an ancestor of the fetch, so the shared prefix guards both
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
//! Past the shared levels the copies are read one at a time, so a value split into copies, each
//! narrowed to some of the types a level allows, serves no configuration even where between them
//! they cover every type: a conservative limit, whether a fetch wrote the split or a path condition
//! guarding a bare field made it.
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
//! # Soundness: the argument has to be bound
//!
//! Every `@fromContext` argument of every field a fetch's operation selects, at every runtime type
//! the position can hold in its subgraph, has to be passed a variable one of the fetch's context
//! rewrites binds ([`check_context_arguments`], the model's `RequiredContextBindings`). Every fetch
//! owes it, a root fetch included, as it owes that its subgraph can resolve what it selects.
//!
//! # Soundness: the rewrite has to read what the declaration asks for
//!
//! The value being there is not the value being right. A rewrite is a path the planner writes from
//! the argument's declaration: as many `Parent`s as levels from the fetch's position up to the
//! object that sets the context, a `TypenameEquals` per runtime type that object can have unless
//! it is the query root, then the declaration's selection down to a leaf. The router reads it key
//! by key in the response, and a key holds whatever an earlier fetch selected under it, whichever
//! field that was; nothing at run time compares what it reads with the declaration. So every
//! rewrite binding a contextual argument's variable is read back against the declaration
//! ([`check_context_readings`], the model's `ContextReadings`): its path is resolved against the
//! fetch's and followed through what was fetched as availability follows it, each copy carrying
//! the field each key stands for, and in each copy an execution of the fetch reads, the copies
//! admitting a runtime configuration of its position as availability admits them, the object the
//! climb reaches has a runtime type some type declaring the context admits, on itself, an
//! interface it implements or a union it belongs to, as the query graph matches a context, and the
//! fields under the copy's keys below it read, by name, down to a leaf of the declaration's
//! selection at that type. At the fetch's own position, which no configuration reaches, the object
//! is any type the copy has there among those the fetch's `requires` entries are written at.
//!
//! The declaring types and the declaration are the subgraph's, where `@context` and `@fromContext`
//! are declared; the members a declaring type admits, the fields the selection names and the types
//! they return are read in the supergraph, where the fetch's position and the data it reads are
//! written. The resolution check, by contrast, reads the subgraph's types throughout. A declaration
//! is one composition accepts, which validates its selection at every type declaring the context,
//! so the reading is the same at each declaring type admitting the object, and the first in the
//! order the subgraph holds them is the one read.
//!
//! Which object setting the context is the nearest is not asked: a rewrite climbing past the
//! nearest object setting it to another that sets it too reads a value of the declared shape from
//! the wrong object, and passes.

#[cfg(test)]
#[path = "context_test.rs"]
mod context_test;

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::conditions::BooleanCondition;
use super::super::query_compare::conditions::BooleanLiteral;
use super::super::query_compare::conditions::boolean_condition_covered_by;
use super::super::query_compare::conditions::canonical_boolean_condition;
use super::super::query_compare::conditions::literals_for_directives;
use super::ComparisonError;
use super::display_path;
use super::selections::TypeCondition;
use super::selections::admitted_types;
use super::selections::possible_types;
use super::subgraph::ContextArgument;
use super::subgraph::Subgraph;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
use crate::query_plan::requires_selection;
use crate::schema::ValidFederationSchema;
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;

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
#[derive(Clone)]
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
    /// interface's or a union's, keeps nothing, so a condition naming no type the level holds
    /// narrows it to none, and a value read below it is read nowhere.
    fn narrow_by_condition(&self, type_condition: Option<&TypeCondition>) -> Self {
        let Some(type_names) = type_condition else {
            return self.clone();
        };
        self.narrowed_to(type_names)
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

/// What guards one copy of a value a path reached.
///
/// Boolean literals are global, so they are one conjunction for the whole path; type conditions
/// are not, each being about the object at one level of it, so they are kept level by level.
#[derive(Clone)]
pub(super) struct ReachedCondition {
    /// The levels already descended past, outermost first.
    levels: Vec<LevelTypes>,
    /// The level the copy sits at.
    current: LevelTypes,
    condition: BooleanCondition,
    /// The field each key the path descended past, or ended at, stands for in this copy, outermost
    /// first: what a rewrite reading that key is sent.
    fields: Vec<Name>,
}

impl ReachedCondition {
    /// Extends a copy's condition by the literals one selection's directives stand for, kept
    /// canonical.
    ///
    /// `None` when they can never all hold, which is no copy at all: `literals_for_directives`
    /// rejects a directive that is false outright, and canonicalization a literal that meets its
    /// complement further up the path.
    fn under(&self, directives: &ast::DirectiveList) -> Option<ReachedCondition> {
        let literals = literals_for_directives(directives)?;
        let mut combined = self.condition.clone();
        combined.extend(literals);
        Some(ReachedCondition {
            levels: self.levels.clone(),
            current: self.current.clone(),
            condition: canonical_boolean_condition(&combined)?,
            fields: self.fields.clone(),
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
}

/// The conditions under which a fetch data path is there to be read, one entry per copy of what it
/// addresses in what the plan has already fetched. Empty when the path addresses nothing.
pub(super) fn path_conditions(
    schema: &ValidFederationSchema,
    root_type: &Name,
    available: &[Selection],
    path: &[FetchDataPathElement],
) -> Vec<ReachedCondition> {
    let reached = ReachedCondition {
        levels: Vec::new(),
        current: LevelTypes::of(schema, root_type),
        condition: BooleanCondition::new(),
        fields: Vec::new(),
    };
    conditions_reached(schema, root_type, &reached, path, available)
}

/// A `TypenameEquals` narrows the level it stands at, the rewrite reading the value only off data
/// of that runtime type. A copy narrowed to nothing at any level is no copy at all, the level the
/// path ends at included: a type guard after the last key that nothing there matches reaches
/// nothing, as the router's path reader, finding no object matching it, supplies no argument.
fn conditions_reached(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    reached: &ReachedCondition,
    path: &[FetchDataPathElement],
    selections: &[Selection],
) -> Vec<ReachedCondition> {
    let Some((head, rest)) = path.split_first() else {
        if reached.current.narrowed.is_empty() {
            return Vec::new();
        }
        return vec![reached.clone()];
    };
    let narrowed = |current: LevelTypes| ReachedCondition {
        levels: reached.levels.clone(),
        current,
        condition: reached.condition.clone(),
        fields: reached.fields.clone(),
    };
    match head {
        FetchDataPathElement::Key(name, type_condition) => reached_under_key(
            schema,
            parent_type,
            reached,
            name,
            type_condition.as_deref(),
            rest,
            selections,
        ),
        FetchDataPathElement::TypenameEquals(type_name) => conditions_reached(
            schema,
            parent_type,
            &narrowed(reached.current.narrow_by(schema, type_name)),
            rest,
            selections,
        ),
        FetchDataPathElement::AnyIndex(type_condition) => conditions_reached(
            schema,
            parent_type,
            &narrowed(
                reached
                    .current
                    .narrow_by_condition(type_condition.as_deref()),
            ),
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
/// it, a fragment naming no type keeping the enclosing one; below a union, whose members alone
/// declare the fields a path descends past, such a key is found inside the fragments on its
/// members. A selection whose directives can never all hold contributes no copy. The field a
/// matched key stands for is recorded, whether the path ends at it or descends past it.
#[allow(clippy::too_many_arguments)]
fn reached_under_key(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    reached: &ReachedCondition,
    name: &Name,
    type_condition: Option<&TypeCondition>,
    path: &[FetchDataPathElement],
    selections: &[Selection],
) -> Vec<ReachedCondition> {
    let mut out = Vec::new();
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if field.response_name() != name || reached.current.narrowed.is_empty() {
                    continue;
                }
                let Some(mut carried) = reached.under(&field.directives) else {
                    continue;
                };
                carried.fields.push(field.name.clone());
                if path.is_empty() {
                    out.push(carried);
                    continue;
                }
                let Some(child_type) = field_output_type(schema, parent_type, &field.name) else {
                    continue;
                };
                let level = LevelTypes::of(schema, &child_type).narrow_by_condition(type_condition);
                out.extend(conditions_reached(
                    schema,
                    &child_type,
                    &carried.descend(level),
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

/// Does a copy of a value serve one runtime configuration?
///
/// At every level the two paths share it must admit the type the data has there, and past those
/// levels it must be unnarrowed, nothing about the fetch saying what those objects are.
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
/// one runtime configuration: the copies of every rewrite that binds the variable and admits that
/// configuration.
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
        let shared = shared_levels(&merged, path);
        let runtime_types = &runtime_types[..runtime_types.len().min(shared)];
        clauses.extend(
            path_conditions(schema, root_type, available, &merged)
                .into_iter()
                .filter(|value| type_levels_admit(&value.type_levels(), runtime_types))
                .map(|value| value.condition),
        );
    }
    clauses
}

/// Is every contextual value a fetch reads already fetched wherever the fetch runs?
///
/// Deciding this per copy of the fetch's own position is what makes the shared prefix cancel; see
/// the module docs.
pub(super) fn check_context_rewrites(
    schema: &ValidFederationSchema,
    root_type: &Name,
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &[Selection],
    readable: &[Selection],
    fetch: &FetchNode,
) -> Result<(), ComparisonError> {
    let mut variables = context_variables(&fetch.context_rewrites);
    // Several rewrites can bind one variable; the check is the same for each, so ask it once.
    variables.dedup();
    if variables.is_empty() {
        return Ok(());
    }
    // Where the fetch runs is found in everything fetched before it; the values it reads, in what
    // it can read. The two differ only inside a deferred block.
    let positions = path_conditions(schema, root_type, available, path);
    for variable in &variables {
        let shared = context_shared_levels(path, &fetch.context_rewrites, variable);
        for fetch_condition in &positions {
            for runtime_types in runtime_combinations(shared, &fetch_condition.type_levels()) {
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
                running.extend(fetch_condition.condition.iter().cloned());
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
// Soundness: every contextual argument is bound
//==================================================================================================

/// A `@fromContext` argument met on a field a fetch's operation selects, with the variable the
/// operation passes it, if it passes a variable at all: the model's `ContextUse`. The type and
/// field it was met on are kept for the explanation.
pub(super) struct ContextUse {
    object_type: Name,
    field: Name,
    declaration: ContextArgument,
    variable: Option<Name>,
}

/// The contextual uses of a fetch's operation, from its root type down: the model's
/// `fetchContextUses`, collecting what `SelectionUsesContext` states. The runtime types below a
/// field are pooled, so a type reached through several parents is visited once, and each argument
/// passed one variable is kept once, the obligations being stated per use.
pub(super) fn context_uses(
    subgraph: &Subgraph<'_>,
    operation: &executable::SelectionSet,
) -> Result<Vec<ContextUse>, ComparisonError> {
    let mut uses = Vec::new();
    selection_context_uses(
        subgraph,
        std::slice::from_ref(&operation.ty),
        &operation.selections,
        &mut uses,
    )?;
    Ok(uses)
}

/// The model's `selectionContextUses`, over a pool of runtime types.
fn selection_context_uses(
    subgraph: &Subgraph<'_>,
    types: &[Name],
    selections: &[Selection],
    uses: &mut Vec<ContextUse>,
) -> Result<(), ComparisonError> {
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if field.name == *INTROSPECTION_TYPENAME_FIELD_NAME {
                    continue;
                }
                let mut below: Vec<Name> = Vec::new();
                for object_type in types {
                    let Some(definition) = subgraph.field(object_type, &field.name) else {
                        continue;
                    };
                    for declaration in subgraph.context_arguments(object_type, &field.name)? {
                        let variable = argument_variable(&field.arguments, &declaration.name);
                        let known = uses.iter().any(|known| {
                            known.declaration == declaration && known.variable == variable
                        });
                        if !known {
                            uses.push(ContextUse {
                                object_type: object_type.clone(),
                                field: field.name.clone(),
                                declaration,
                                variable,
                            });
                        }
                    }
                    for child in possible_types(subgraph.schema(), definition.ty.inner_named_type())
                    {
                        if !below.contains(&child) {
                            below.push(child);
                        }
                    }
                }
                if !field.selection_set.selections.is_empty() {
                    selection_context_uses(
                        subgraph,
                        &below,
                        &field.selection_set.selections,
                        uses,
                    )?;
                }
            }
            Selection::InlineFragment(fragment) => {
                let admitted =
                    admitted_types(subgraph.schema(), fragment.type_condition.as_slice());
                let inside: Vec<Name> = types
                    .iter()
                    .filter(|object_type| {
                        admitted
                            .as_ref()
                            .is_none_or(|types| types.contains(object_type))
                    })
                    .cloned()
                    .collect();
                selection_context_uses(
                    subgraph,
                    &inside,
                    &fragment.selection_set.selections,
                    uses,
                )?;
            }
            // Spreads are inlined before a fetch's operation reaches here.
            Selection::FragmentSpread(_) => {}
        }
    }
    Ok(())
}

/// The variable an argument is passed, read at its first occurrence, which validity makes its only
/// one: the model's `argumentVariable`.
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

/// Is every contextual argument the fetch's operation passes passed a variable one of its context
/// rewrites binds? The model's `RequiredContextBindings`.
///
/// The planner writes the argument and the rewrite together, as
/// `add_context_renamers_for_selection_set` does, and the value reaches the subgraph no other way:
/// a fetch that drops both sends the field with no value, which no check of the rewrites it does
/// declare can see, and a variable no rewrite binds, or a literal, is no value from the context.
pub(super) fn check_context_arguments(
    fetch: &FetchNode,
    path: &[FetchDataPathElement],
    uses: &[ContextUse],
    context_variables: &[Name],
) -> Result<(), ComparisonError> {
    for context_use in uses {
        let unbound = match &context_use.variable {
            Some(variable) if context_variables.contains(variable) => continue,
            Some(variable) => {
                format!("passes it `${variable}`, which none of its context rewrites binds")
            }
            None => "passes it no variable a context rewrite binds".to_string(),
        };
        return Err(ComparisonError::new(format!(
            "fetch to {}: `{}.{}` takes `{}` from context `{}`, and the fetch {unbound}\n\
             * fetch runs at: {}",
            fetch.subgraph_name,
            context_use.object_type,
            context_use.field,
            context_use.declaration.name,
            context_use.declaration.context,
            display_path(path),
        )));
    }
    Ok(())
}

//==================================================================================================
// Soundness: what a rewrite reads is what the declaration asks for
//==================================================================================================

/// Does every rewrite binding the variable a contextual argument is passed read what the argument's
/// declaration asks for? The model's `ContextReadings`.
///
/// Each such rewrite is held to it, availability being a disjunction over the rewrites binding a
/// variable and this not; a rewrite binding a variable no argument passes has no declaration to be
/// read against. `schema` is the supergraph's, where the fetch's position and the data it reads
/// are written.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_context_readings(
    schema: &ValidFederationSchema,
    subgraph: &Subgraph<'_>,
    root_type: &Name,
    path: &[FetchDataPathElement],
    available: &[Selection],
    fetch: &FetchNode,
    uses: &[ContextUse],
) -> Result<(), ComparisonError> {
    let mut fetch_positions = None;
    for context_use in uses {
        let Some(variable) = &context_use.variable else {
            continue;
        };
        for rewrite in &fetch.context_rewrites {
            let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
                continue;
            };
            if renamer.rename_key_to != *variable {
                continue;
            }
            // Where the fetch runs, found as availability finds it, once per fetch.
            let positions = fetch_positions
                .get_or_insert_with(|| path_conditions(schema, root_type, available, path));
            check_context_rewrite_reads(
                schema,
                subgraph,
                root_type,
                path,
                available,
                fetch,
                positions,
                context_use,
                &renamer.path,
            )?;
        }
    }
    Ok(())
}

/// Does one rewrite read what one declaration asks for? The model's `ContextRewriteReads`.
///
/// In every runtime configuration of the fetch's position over the levels it shares with the
/// rewrite's path, in every copy of the value an execution there reads, for the runtime type the
/// object the rewrite climbs to has there, a type declaring the context admits that type, the first
/// in declaration order, and the fields the copy's keys stand for below that object read a leaf of
/// the declaration at that type. A rewrite reading no copy is held to nothing here; whether it
/// reads one is availability's question.
#[allow(clippy::too_many_arguments)]
fn check_context_rewrite_reads(
    schema: &ValidFederationSchema,
    subgraph: &Subgraph<'_>,
    root_type: &Name,
    path: &[FetchDataPathElement],
    available: &[Selection],
    fetch: &FetchNode,
    positions: &[ReachedCondition],
    context_use: &ContextUse,
    context_path: &[FetchDataPathElement],
) -> Result<(), ComparisonError> {
    let Some(merged) = merge_context_path(path, context_path) else {
        return Ok(());
    };
    let climbed = context_climb(context_path);
    // `merge_context_path` climbs one key per `Parent`, so the fetch's path has as many.
    let depth = fetch_path_depth(path) - climbed;
    let shared = shared_levels(&merged, path);
    let copies = path_conditions(schema, root_type, available, &merged);
    let declaring = subgraph.context_types(&context_use.declaration.context)?;
    let misread = |reason: String| {
        ComparisonError::new(format!(
            "fetch to {}: `{}.{}` takes `{}` from context `{}`, and the context rewrite binding \
             `${}` reads `{}`, {reason}\n* fetch runs at: {}",
            fetch.subgraph_name,
            context_use.object_type,
            context_use.field,
            context_use.declaration.name,
            context_use.declaration.context,
            context_use
                .variable
                .as_ref()
                .map_or("", |variable| variable.as_str()),
            display_path(context_path),
            display_path(path),
        ))
    };
    for position in positions {
        for runtime_types in runtime_combinations(shared, &position.type_levels()) {
            for copy in context_read_copies(&copies, &runtime_types) {
                let levels = copy.type_levels();
                for object_type in
                    context_object_types(schema, fetch, climbed, depth, &runtime_types, &levels)
                {
                    let Some(declaring_type) = declaring.iter().find(|declaring_type| {
                        possible_types(schema, declaring_type).contains(&object_type)
                    }) else {
                        return Err(misread(format!(
                            "which climbs to `{object_type}`, a type declaring no context `{}` \
                             in the subgraph",
                            context_use.declaration.context
                        )));
                    };
                    let fields = copy.fields.get(depth..).unwrap_or_default();
                    let below = levels.get(depth + 1..).unwrap_or_default();
                    let object = LevelTypes {
                        allowed: vec![object_type.clone()],
                        narrowed: vec![object_type.clone()],
                    };
                    if !reads_declared_leaf(
                        schema,
                        declaring_type,
                        &object,
                        &context_use.declaration.selection,
                        fields,
                        below,
                    ) {
                        return Err(misread(format!(
                            "which on `{object_type}` holds `{}`, not a leaf of the declaration's \
                             selection there",
                            fields
                                .iter()
                                .map(|field| field.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// How many levels a rewrite climbs: its leading `Parent`s, which `merge_context_path` resolves
/// against the fetch's path. The model's `contextClimb`.
fn context_climb(context_path: &[FetchDataPathElement]) -> usize {
    context_path
        .iter()
        .take_while(|element| matches!(element, FetchDataPathElement::Parent))
        .count()
}

/// How many levels below the query root a fetch runs at: one per response key of its path, an index
/// naming no level. The model's `fetchPathDepth`.
fn fetch_path_depth(path: &[FetchDataPathElement]) -> usize {
    path.iter()
        .filter(|element| matches!(element, FetchDataPathElement::Key(..)))
        .count()
}

/// The copies of a value a rewrite's path reaches that an execution of the fetch at one runtime
/// configuration of its position reads: those whose levels admit the configuration, as
/// [`context_value_clauses`] admits a copy for availability. A copy under a runtime type the fetch
/// never runs at is not among them. The model's `contextReadCopies`.
fn context_read_copies<'c>(
    copies: &'c [ReachedCondition],
    runtime_types: &[Name],
) -> Vec<&'c ReachedCondition> {
    copies
        .iter()
        .filter(|copy| type_levels_admit(&copy.type_levels(), runtime_types))
        .collect()
}

/// The runtime types one copy gives the object a rewrite climbed to, in one configuration of the
/// fetch's position: the configuration's type at the level the climb reaches, `depth` keys below
/// the root, which the copy admits there; at the fetch's own position, which a configuration does
/// not reach, the copy's own types there, narrowed to the ones the fetch's `requires` entries are
/// written at, an entity of no other type being sent. The model's `contextObjectTypes`.
fn context_object_types(
    schema: &ValidFederationSchema,
    fetch: &FetchNode,
    climbed: usize,
    depth: usize,
    runtime_types: &[Name],
    levels: &[&LevelTypes],
) -> Vec<Name> {
    let reached = levels
        .get(depth)
        .map(|level| level.narrowed.clone())
        .unwrap_or_default();
    if climbed > 0 {
        return reached
            .into_iter()
            .filter(|type_name| runtime_types.get(depth) == Some(type_name))
            .collect();
    }
    if fetch.requires.is_empty() {
        return reached;
    }
    let entry_types: Vec<Name> = fetch
        .requires
        .iter()
        .filter_map(|entry| match entry {
            requires_selection::Selection::InlineFragment(fragment) => {
                fragment.type_condition.as_ref()
            }
            requires_selection::Selection::Field(_) => None,
        })
        .flat_map(|type_name| possible_types(schema, type_name))
        .collect();
    reached
        .into_iter()
        .filter(|type_name| entry_types.contains(type_name))
        .collect()
}

/// Do the fields a copy's keys stand for below the object a rewrite climbed to read a leaf of a
/// declaration's selection? The model's `ReadsDeclaredLeaf`.
///
/// Each field is a field of that name the declaration selects at this level, for data of the
/// runtime types `types` holds, directly or under a fragment admitting one of them, and the last
/// selects nothing below it. `levels` are the copy's levels below the first field, one per field
/// but the last, and say what runtime types the data below each field can have. The keys'
/// spelling decides nothing: a key holds whatever field an earlier fetch selected under it.
fn reads_declared_leaf(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    types: &LevelTypes,
    selections: &[Selection],
    fields: &[Name],
    levels: &[&LevelTypes],
) -> bool {
    let Some((field_name, rest)) = fields.split_first() else {
        return false;
    };
    declared_fields_named(schema, parent_type, types, field_name, selections)
        .into_iter()
        .any(
            |(field_type, below)| match (rest.is_empty(), levels.split_first()) {
                (true, _) => below.is_empty(),
                (false, None) => false,
                (false, Some((level, level_rest))) => {
                    reads_declared_leaf(schema, &field_type, level, below, rest, level_rest)
                }
            },
        )
}

/// The fields of a name a declaration selects at one level, for data of the runtime types `types`
/// holds, each with its own type and what is selected below it: directly, or under an inline
/// fragment admitting one of the types, which is where the static parent changes. The model's
/// `declaredFieldsNamed`.
fn declared_fields_named<'s>(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    types: &LevelTypes,
    field_name: &Name,
    selections: &'s [Selection],
) -> Vec<(Name, &'s [Selection])> {
    let mut found = Vec::new();
    for selection in selections {
        match selection {
            Selection::Field(field) if field.name == *field_name => {
                if let Some(field_type) = field_or_typename_type(schema, parent_type, &field.name) {
                    found.push((field_type, field.selection_set.selections.as_slice()));
                }
            }
            Selection::Field(_) => {}
            Selection::InlineFragment(fragment) => {
                let narrowed = match &fragment.type_condition {
                    Some(type_condition) => types.narrow_by(schema, type_condition),
                    None => types.clone(),
                };
                if !narrowed.narrowed.is_empty() {
                    found.extend(declared_fields_named(
                        schema,
                        fragment.type_condition.as_ref().unwrap_or(parent_type),
                        &narrowed,
                        field_name,
                        &fragment.selection_set.selections,
                    ));
                }
            }
            // A field value has no named fragments.
            Selection::FragmentSpread(_) => {}
        }
    }
    found
}

/// The named type of a field of a type, or `String` for the `__typename` every object, interface
/// and union has though none declares it. It is the only field a declaration on a union selects
/// directly; the declaration can still select its members' fields through fragments. The model's
/// `lookupFieldOrTypename`.
fn field_or_typename_type(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    field_name: &Name,
) -> Option<Name> {
    field_output_type(schema, parent_type, field_name).or_else(|| {
        let composite = matches!(
            schema.schema().types.get(parent_type),
            Some(ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_))
        );
        (*field_name == *INTROSPECTION_TYPENAME_FIELD_NAME && composite).then(|| name!("String"))
    })
}
