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
//! judged not to, on an argument mismatch. What a fetch produces is therefore read with them
//! dropped, as `remove_context_arguments` drops them from the fetch's response shape; every other
//! argument stays and is compared.
//!
//! # Soundness: the value has to be there
//!
//! At run time `subgraph_context.rs` resolves a rewrite's path against the path the fetch runs at
//! and reads the data. A path reaching nothing is not an error there — the argument is simply
//! never bound — so nothing notices that the plan asked for data it never fetched. This checks it,
//! which the legacy checker does not.
//!
//! The fetch runs on the copy its own path reaches in what was fetched before it, in the
//! configurations its path and its condition allow; the value is read from what the fetch can
//! read. The obligation is the model's `FetchSiteContextMet`: wherever the fetch runs, whatever
//! runtime type every copy of the response has, some rewrite binding the variable reads a fetched
//! value. A rewrite's path shares the fetch's own copies over the levels the two paths share
//! ([`shared_levels`]): those are the same copies, and the same variables of the configurations,
//! so the value is read under the very parent the fetch runs under. Past them it reads copies of
//! its own, a sibling branch's, whose runtime types are as independent of the fetch's copies at
//! equal depth as any other copy's: the value has to be there whichever runtime type each has,
//! and whether or not it is there at all. Complete coverage split among a copy's runtime types is
//! accepted, and a runtime type left without the value is not ([`check_context_rewrites`]).
//!
//! One variable at a time, not one rewrite at a time. A fetch can carry several rewrites binding
//! the same variable, one per runtime type the context object may have, and the router takes the
//! value from whichever resolves — `execute_on_path` claims the variable only once a rewrite has
//! produced a value, so one that finds nothing falls through to the next. A rewrite whose path
//! does not resolve contributes nothing rather than failing on its own.
//!
//! A merged path is spliced from two kinds of path that carry their type conditions differently —
//! a flatten path on its elements, a rewrite path as `TypenameEquals` — and both are read
//! literally, against the object's runtime type at that level, in the supergraph.
//!
//! What the obligation does not reach: a fetch that requires nothing is not asked its contextual
//! availability, and nothing enforces that it carries no context rewrites, the planner writing
//! them only on a fetch given its key inputs and the router resolving none for a fetch that
//! requires nothing, while the readings below still judge any it carries; a subscription's primary
//! fetch is outside what the model states. Whether the router answers a list holding both kinds of
//! member correctly is shown by no run: it collects each object's context values before it filters
//! and deduplicates the representations, and these checks establish neither that the two stay
//! aligned nor the list's response.
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
//! ([`check_context_readings`], the model's `ContextReadings`): on every copy the fetch runs on,
//! every value the rewrite reads there that availability would read, on the same copies and under
//! the same assignments, is held to it. The copy the climb reaches is the fetch's own position's
//! ancestor at that level, the copy the fetch runs on included, and its runtime type must
//! be one a type declaring the context admits, on itself, an interface it implements or a union it
//! belongs to, as the query graph matches a context; and the fields under the value's keys below it
//! must read, by name, down to a leaf of the declaration's selection at that type, each level read
//! at the runtime type of the copy there.
//!
//! The declaring types and the declaration are the subgraph's, where `@context` and `@fromContext`
//! are declared; the members a declaring type admits, the fields the selection names and the types
//! they return are read in the supergraph, where the fetch's position and the data it reads are
//! written, and so are the object types a `requires` entry's type condition stands for, an
//! interface's being its object types, where a path element's own condition is compared literally.
//! The resolution check, by contrast, reads the subgraph's types throughout. A declaration is one
//! composition accepts, which validates its selection at every type declaring the context, so the
//! reading is the same at each declaring type admitting the object, and the first in the order the
//! subgraph holds them is the one read.
//!
//! Which object setting the context is the nearest is not asked: a rewrite climbing past the
//! nearest object setting it to another that sets it too reads a value of the declared shape from
//! the wrong object, and passes.

#[cfg(test)]
#[path = "context_test.rs"]
mod context_test;

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::schema_view::SchemaView;
use super::ComparisonError;
use super::Site;
use super::coverage::render_clause;
use super::display_path;
use super::region::Algebra;
use super::region::Clause;
use super::region::Region;
use super::region::Value;
use super::region::Var;
use super::region::everything;
use super::region::vars;
use super::response::CopyId;
use super::response::View;
use super::response::every_object;
use super::selections::admitted_types;
use super::selections::possible_types;
use super::subgraph::ContextArgument;
use super::subgraph::Subgraph;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
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
// Soundness: the obligation
//==================================================================================================

/// One rewrite's path, followed through what the fetch can read: the model's `pathConditions`
/// with `PathEnd.value`.
struct Followed {
    /// The copies its keys lead to, in order: those past the levels it shares with the fetch's own
    /// path are copies of their own, each a variable of the obligation.
    copies: Vec<CopyId>,
    /// The copy holding the value: the one the path ends at, or the one whose key holds it.
    copy: CopyId,
    /// The field names under its keys, one choice per contribution read, each with the
    /// configurations in which the value is read through them: the path's type conditions, each
    /// key's contributions active, and the `__typename` value each condition on a key or an
    /// index reads, fetched.
    readings: Vec<(Vec<Name>, Region)>,
}

/// Follows a merged rewrite path from the root through `readable`. `None` where it reaches no copy
/// at all.
fn follow(
    algebra: &Algebra,
    readable: &View<'_>,
    path: &[FetchDataPathElement],
) -> Option<Followed> {
    let response = readable.response;
    let domain = every_object(algebra);
    let mut followed = Followed {
        copies: Vec::new(),
        copy: CopyId::ROOT,
        readings: vec![(Vec::new(), everything())],
    };
    let narrow =
        |readings: Vec<(Vec<Name>, Region)>, region: &Region| -> Vec<(Vec<Name>, Region)> {
            readings
                .into_iter()
                .filter_map(|(fields, reading)| {
                    let reading = algebra.simplify(algebra.product(&reading, region), &domain);
                    (!reading.is_empty()).then_some((fields, reading))
                })
                .collect()
        };
    for (at, element) in path.iter().enumerate() {
        algebra.work.path_steps.add(1);
        let condition = match element {
            FetchDataPathElement::Key(name, condition) => {
                let mut by_field: Vec<(Name, Region)> = Vec::new();
                for field in readable.fields_at(followed.copy, name) {
                    algebra.work.path_contributions.add(1);
                    match by_field.iter_mut().find(|(known, _)| *known == field.field) {
                        Some((_, guards)) => guards.extend(field.guard.iter().cloned()),
                        None => by_field.push((field.field.clone(), field.guard.clone())),
                    }
                }
                let mut readings = Vec::new();
                for (fields, reading) in followed.readings {
                    for (field, guards) in &by_field {
                        let reading = algebra.simplify(algebra.product(&reading, guards), &domain);
                        if reading.is_empty() {
                            continue;
                        }
                        let mut fields = fields.clone();
                        fields.push(field.clone());
                        readings.push((fields, reading));
                    }
                }
                followed.readings = readings;
                if at + 1 == path.len() && condition.is_none() {
                    break;
                }
                let child = response.child(followed.copy, name)?;
                followed.copies.push(child);
                followed.copy = child;
                condition
            }
            FetchDataPathElement::AnyIndex(condition) => condition,
            FetchDataPathElement::TypenameEquals(type_name) => {
                let literal =
                    response.type_literal(algebra, followed.copy, algebra.objects([type_name]))?;
                followed.readings = narrow(followed.readings, &vec![literal]);
                continue;
            }
            FetchDataPathElement::Parent => return None,
        };
        if let Some(names) = condition {
            let literal = response.type_literal(algebra, followed.copy, algebra.objects(names))?;
            let observed: Region = readable.typename_guards(followed.copy);
            followed.readings = narrow(followed.readings, &vec![literal]);
            followed.readings = narrow(followed.readings, &observed);
        }
    }
    (!followed.readings.is_empty()).then_some(followed)
}

/// Is every contextual value a fetch reads already fetched wherever the fetch runs? The model's
/// `FetchSiteContextMet`.
///
/// The fetch runs on the copy its path addresses in what was fetched before it, in the site's
/// region; each rewrite binding a variable is followed through what the fetch can read, and
/// wherever the fetch runs, some rewrite has to read a fetched value: the rewrites binding one
/// variable form a disjunction, each configuration finding its value through any of them. The
/// copies a rewrite reads past the levels it shares with the fetch's own path are their own
/// variables, as independent of the fetch's copies at equal depth as any other copy, and are
/// quantified like the fetch's: a value read off one of them has to be there whichever runtime type
/// it has, or whether it is there at all.
pub(super) fn check_context_rewrites(site: &Site<'_>) -> Result<(), ComparisonError> {
    let hypothesis = site.hypothesis;
    let algebra = hypothesis.algebra;
    let response = site.readable.response;
    let fetch = site.fetch;
    let mut variables = context_variables(&fetch.context_rewrites);
    // Several rewrites can bind one variable; the check is the same for each, so ask it once.
    variables.dedup();
    for variable in &variables {
        let mut branches: Vec<CopyId> = Vec::new();
        let mut supplies: Vec<Clause> = Vec::new();
        for rewrite in &fetch.context_rewrites {
            let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
                continue;
            };
            if renamer.rename_key_to != *variable {
                continue;
            }
            let Some(merged) = merge_context_path(site.path, &renamer.path) else {
                continue;
            };
            let shared = shared_levels(&merged, site.path);
            let Some(followed) = follow(algebra, &site.readable, &merged) else {
                continue;
            };
            // A value under fewer of the fetch's own copies than the paths share is not read.
            if response.ancestors(followed.copy).len() < shared {
                continue;
            }
            branches.extend(followed.copies);
            for (_, reading) in followed.readings {
                supplies.extend(reading);
            }
        }
        let supplies = hypothesis.supplies(supplies);
        if let Some(example) =
            hypothesis.uncovered_in_tree(site.position.copy, &branches, &site.region, &supplies)
        {
            return Err(ComparisonError::new(format!(
                "fetch to {}: the contextual value for `${variable}` is not always fetched \
                 where the fetch runs\n* fetch runs at: {}, at {}\n* not read{}",
                fetch.subgraph_name,
                display_path(site.path),
                response.describe(site.position.copy),
                if example.is_empty() {
                    " under any condition".to_string()
                } else {
                    render_clause(algebra, response, &example)
                },
            )));
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
/// read against. The fetch's position and the configurations it runs in are availability's, and
/// the values the rewrites reach are found in what the fetch can read.
pub(super) fn check_context_readings(
    site: &Site<'_>,
    subgraph: &Subgraph<'_>,
    uses: &[ContextUse],
) -> Result<(), ComparisonError> {
    for context_use in uses {
        let Some(variable) = &context_use.variable else {
            continue;
        };
        for rewrite in &site.fetch.context_rewrites {
            let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
                continue;
            };
            if renamer.rename_key_to != *variable {
                continue;
            }
            check_context_rewrite_reads(site, subgraph, context_use, &renamer.path)?;
        }
    }
    Ok(())
}

/// Does one rewrite read what one declaration asks for? The model's `ContextRewriteReads`.
///
/// Wherever the fetch runs and the rewrite reads a value, on the same copies as the fetch's own
/// position over the levels the two paths share: the copy the climb reaches, the fetch's own
/// position's ancestor that many levels down, has a runtime type a type declaring the context
/// admits, the first in declaration order, and the fields the value's keys stand for below that
/// copy read a leaf of the declaration at that type, each level read at the runtime type of the
/// copy there. Every combination of those runtime types the configurations allow is read. A
/// rewrite reading no value is held to nothing here; whether it reads one is availability's
/// question.
fn check_context_rewrite_reads(
    site: &Site<'_>,
    subgraph: &Subgraph<'_>,
    context_use: &ContextUse,
    context_path: &[FetchDataPathElement],
) -> Result<(), ComparisonError> {
    let hypothesis = site.hypothesis;
    let algebra = hypothesis.algebra;
    let response = site.readable.response;
    let schema = site.schema;
    let Some(merged) = merge_context_path(site.path, context_path) else {
        return Ok(());
    };
    let climbed = context_climb(context_path);
    // `merge_context_path` climbs one key per `Parent`, so the fetch's path has as many.
    let depth = fetch_path_depth(site.path) - climbed;
    let shared = shared_levels(&merged, site.path);
    let chain = response.ancestors(site.position.copy);
    // The copy the climb reaches; a position that does not reach that level reads nothing.
    let Some(setter) = chain.get(depth).copied() else {
        return Ok(());
    };
    let Some(followed) = follow(algebra, &site.readable, &merged) else {
        return Ok(());
    };
    let copy_chain = response.ancestors(followed.copy);
    if copy_chain.len() < shared {
        return Ok(());
    }
    let declaring = subgraph.context_types(&context_use.declaration.context)?;
    let misread = |reason: String| {
        ComparisonError::new(format!(
            "fetch to {}: `{}.{}` takes `{}` from context `{}`, and the context rewrite binding \
             `${}` reads `{}`, {reason}\n* fetch runs at: {}",
            site.fetch.subgraph_name,
            context_use.object_type,
            context_use.field,
            context_use.declaration.name,
            context_use.declaration.context,
            context_use
                .variable
                .as_ref()
                .map_or("", |variable| variable.as_str()),
            display_path(context_path),
            display_path(site.path),
        ))
    };
    // The copies whose runtime types the reading reads: the setter and the levels below it.
    let mut read: Vec<CopyId> = vec![setter];
    read.extend(copy_chain.iter().skip(depth + 1).copied());
    let read_vars: Vec<Var> = read
        .iter()
        .filter(|copy| **copy != CopyId::ROOT)
        .map(|copy| Var::Copy(*copy))
        .collect();
    let completion = hypothesis.completion();
    let mut judged: HashMap<Vec<Name>, bool> = HashMap::new();
    for (fields, reading) in &followed.readings {
        let fields = fields.get(depth..).unwrap_or_default();
        let mut keep = read_vars.clone();
        keep.extend(vars(&site.region).cloned());
        keep.extend(reading.iter().flat_map(|clause| vars(clause).cloned()));
        keep.sort();
        keep.dedup();
        let tree = hypothesis.tree(site.position.copy, &followed.copies, &keep);
        let region = algebra.product(
            &algebra.product(&tree, std::slice::from_ref(&site.region)),
            reading,
        );
        for clause in region {
            // Every combination of the read copies' runtime types the clause allows.
            let mut tuples: Vec<Vec<Name>> = vec![Vec::new()];
            for copy in &read {
                let types = if *copy == CopyId::ROOT {
                    vec![response.root_type().clone()]
                } else {
                    let set = clause
                        .iter()
                        .find(|literal| literal.var == Var::Copy(*copy))
                        .map(|literal| match literal.value {
                            Value::Types(types) => types,
                            Value::Is(_) => completion(&literal.var),
                        })
                        .unwrap_or_else(|| completion(&Var::Copy(*copy)));
                    algebra.names(set)
                };
                tuples = tuples
                    .into_iter()
                    .flat_map(|tuple| {
                        types.iter().map(move |name| {
                            let mut tuple = tuple.clone();
                            tuple.push(name.clone());
                            tuple
                        })
                    })
                    .collect();
            }
            for tuple in tuples {
                let object_type = &tuple[0];
                let Some(declaring_type) = declaring.iter().find(|declaring_type| {
                    *declaring_type == object_type
                        || schema.possible_types(declaring_type).contains(object_type)
                }) else {
                    return Err(misread(format!(
                        "which climbs to `{object_type}`, a type declaring no context `{}` in the \
                         subgraph",
                        context_use.declaration.context
                    )));
                };
                let mut key = tuple.clone();
                key.extend(fields.iter().cloned());
                let reads = *judged.entry(key).or_insert_with(|| {
                    let levels: Vec<&Name> = tuple[1..].iter().collect();
                    reads_declared_leaf(
                        schema,
                        declaring_type,
                        std::slice::from_ref(object_type),
                        &context_use.declaration.selection,
                        fields,
                        &levels,
                    )
                });
                if !reads {
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

/// Do the fields a value's keys stand for below the copy a rewrite climbed to read a leaf of a
/// declaration's selection? The model's `ReadsDeclaredLeaf`.
///
/// Each field is a field of that name the declaration selects at this level, for data of the
/// runtime types `types` holds, directly or under a fragment admitting one of them, and the last
/// selects nothing below it. `levels` are the runtime types of the copies below the first field,
/// one per field but the last. The keys' spelling decides nothing: a key holds whatever
/// field an earlier fetch selected under it.
fn reads_declared_leaf(
    schema: &SchemaView<'_>,
    parent_type: &Name,
    types: &[Name],
    selections: &[Selection],
    fields: &[Name],
    levels: &[&Name],
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
                (false, Some((level, level_rest))) => reads_declared_leaf(
                    schema,
                    &field_type,
                    std::slice::from_ref(*level),
                    below,
                    rest,
                    level_rest,
                ),
            },
        )
}

/// The fields of a name a declaration selects at one level, for data of the runtime types `types`
/// holds, each with its own type and what is selected below it: directly, or under an inline
/// fragment admitting one of the types, which is where the static parent changes. The model's
/// `declaredFieldsNamed`.
fn declared_fields_named<'s>(
    schema: &SchemaView<'_>,
    parent_type: &Name,
    types: &[Name],
    field_name: &Name,
    selections: &'s [Selection],
) -> Vec<(Name, &'s [Selection])> {
    let mut found = Vec::new();
    for selection in selections {
        match selection {
            Selection::Field(field) if field.name == *field_name => {
                if let Some(field_type) =
                    field_or_typename_type(schema.schema(), parent_type, &field.name)
                {
                    found.push((field_type, field.selection_set.selections.as_slice()));
                }
            }
            Selection::Field(_) => {}
            Selection::InlineFragment(fragment) => {
                let narrowed: Vec<Name> = match &fragment.type_condition {
                    Some(type_condition) => types
                        .iter()
                        .filter(|type_name| {
                            schema.possible_types(type_condition).contains(type_name)
                        })
                        .cloned()
                        .collect(),
                    None => types.to_vec(),
                };
                if !narrowed.is_empty() {
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
