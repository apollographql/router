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

#[cfg(test)]
#[path = "context_test.rs"]
mod context_test;

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::conditions::BooleanCondition;
use super::super::query_compare::conditions::BooleanLiteral;
use super::super::query_compare::conditions::boolean_condition_covered_by;
use super::super::query_compare::conditions::canonical_boolean_condition;
use super::super::query_compare::conditions::literals_for_directives;
use super::ComparisonError;
use super::selections::TypeCondition;
use super::selections::possible_types;
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

    /// Narrows by a path element's own type condition, which is a disjunction.
    fn narrow_by_condition(
        &self,
        schema: &ValidFederationSchema,
        type_condition: Option<&TypeCondition>,
    ) -> Self {
        let Some(type_names) = type_condition else {
            return self.clone();
        };
        let admitted: Vec<Name> = type_names
            .iter()
            .flat_map(|type_name| possible_types(schema, type_name))
            .collect();
        self.narrowed_to(&admitted)
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
    };
    conditions_reached(schema, root_type, &reached, path, available)
}

/// A `TypenameEquals` narrows the level it stands at, the rewrite reading the value only off data
/// of that runtime type; a copy narrowed to nothing is no copy at all.
fn conditions_reached(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    reached: &ReachedCondition,
    path: &[FetchDataPathElement],
    selections: &[Selection],
) -> Vec<ReachedCondition> {
    let Some((head, rest)) = path.split_first() else {
        return vec![reached.clone()];
    };
    let narrowed = |current: LevelTypes| ReachedCondition {
        levels: reached.levels.clone(),
        current,
        condition: reached.condition.clone(),
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
                    .narrow_by_condition(schema, type_condition.as_deref()),
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
/// the level below, where the field's own type takes over. A selection whose directives can never
/// all hold contributes no copy.
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
                if field.response_key() != name || reached.current.narrowed.is_empty() {
                    continue;
                }
                let Some(carried) = reached.under(&field.directives) else {
                    continue;
                };
                if path.is_empty() {
                    out.push(carried);
                    continue;
                }
                let Some(child_type) = field_output_type(schema, parent_type, &field.name) else {
                    continue;
                };
                let level =
                    LevelTypes::of(schema, &child_type).narrow_by_condition(schema, type_condition);
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
                    parent_type,
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
    fetch: &FetchNode,
) -> Result<(), ComparisonError> {
    let mut variables = context_variables(&fetch.context_rewrites);
    // Several rewrites can bind one variable; the check is the same for each, so ask it once.
    variables.dedup();
    if variables.is_empty() {
        return Ok(());
    }
    let positions = path_conditions(schema, root_type, available, path);
    for variable in &variables {
        let shared = context_shared_levels(path, &fetch.context_rewrites, variable);
        for fetch_condition in &positions {
            for runtime_types in runtime_combinations(shared, &fetch_condition.type_levels()) {
                let covers = context_value_clauses(
                    schema,
                    root_type,
                    path,
                    available,
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

fn display_path(path: &[FetchDataPathElement]) -> String {
    path.iter()
        .map(|element| element.to_string())
        .collect::<Vec<_>>()
        .join(".")
}
