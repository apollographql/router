//! Query inclusion: does every response field the right operation produces also appear, from the
//! same resolver call, in the left operation's response?
//!
//! This is a port of `QueryInclusion.includesBool` from the Lean formalization in
//! `graphql-lean` (`GraphQL/Theories/QueryInclusion.lean`), where it carries soundness and
//! completeness statements (`IncludesBoolSound`, `IncludesBoolComplete`) against a semantic
//! relation over concrete annotated executions. The port follows that source closely — function
//! names and the order of checks are kept recognizable — so the two can be differentially tested
//! and so a change on the Lean side can be transcribed here.
//!
//! The algorithm analyzes each response name independently. For one response name it:
//!
//! 1. splits the parent's runtime types into *regions*, refining only by the type conditions that
//!    can affect that response name;
//! 2. case-splits over only the `@skip`/`@include` variables that can affect it;
//! 3. compares the resolver call (field name and arguments) the two sides would make; and
//! 4. recurses into the merged sub-selection of a composite field, over the field's *exact*
//!    return region rather than its declared interface.
//!
//! Two shortcuts bypass the search when a cheaper witness exists: a symbolic clause-subtraction
//! check for scalar fields, and a syntactic inclusion check for one-to-one composite fields.
//! Both are also present in the Lean source and are ported rather than simplified away, since a
//! divergence in either direction would be a divergence from the model.
//!
//! Scope: the model covers `@skip` and `@include` only. Any other directive, and any named
//! fragment spread, is reported rather than ignored — see [`Mismatch::is_inclusion_finding`].
//!
//! # Divergences from the Lean source
//!
//! **No fuel.** The Lean definitions thread a `responseFuel` counter, seeded at `right.size + 1`
//! and spent one unit per nested response boundary, purely to give Lean's termination checker a
//! decreasing measure. It is not a semantic bound and it never binds: response depth drops by at
//! least one at every boundary where fuel does, so `selectionSetResponseDepth right ≤ responseFuel`
//! is an invariant of the recursion rather than a test, and the `0` cases
//! (`guardedScalarFieldIncludesBool`, `guardedCompositeFieldIncludesBool`, and the
//! `rightFields.isEmpty` fallback in `guardedFieldGroupIncludesForRegionsWithFuel`) are
//! unreachable from `includesBool`. Rust needs no such measure, so the counter — and with it the
//! depth guard on the syntactic shortcut — is dropped. Recursion here terminates because every
//! step descends into a strictly smaller sub-selection of the right operation, and rejecting
//! fragment spreads rules out the only way a selection set could refer to itself.
//!
//! **Specialized inherited condition.** `extractFields` threads an `inheritedBooleanCondition`
//! that both entry points pass as empty; see `conditions.rs`.
//!
//! **Canonical region order.** `getPossibleTypes` returns schema declaration order; this port
//! sorts by name. Regions are sets, so the verdict is unaffected, and sorting makes the
//! child-task de-duplication in [`child_tasks_for_parent_types`] canonical.

mod conditions;
mod error;
mod schema_view;

#[cfg(test)]
mod tests;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::validation::Valid;

pub use self::conditions::Assignment;
use self::conditions::BooleanCondition;
use self::conditions::ConditionedField;
use self::conditions::boolean_condition_covered_by;
use self::conditions::of_selection_set;
use self::conditions::of_type_region;
pub use self::error::ComparisonError;
pub use self::error::Mismatch;
pub use self::error::PathSegment;
use self::schema_view::SchemaView;
use super::response_shape_compare::NoConstraint;
use super::response_shape_compare::PathConstraint;
use super::response_shape_compare::PossibleTypes;
use crate::schema::ValidFederationSchema;
use crate::schema::position::ObjectTypeDefinitionPosition;

//==================================================================================================
// Entry point

/// Does `left` include `right`? That is: under every error-free execution, does every response
/// field `right` produces also appear in `left`'s response, produced by the same resolver call?
///
/// `Ok(())` is the inclusion verdict. An `Err` explains where and why inclusion fails — but check
/// [`ComparisonError::is_inclusion_finding`] first: an input outside the modeled fragment is
/// reported through the same channel and says nothing about inclusion.
///
/// The direction is the same as the Lean `includes schema left right`: `left` is the larger
/// query, `right` is the one that must be covered.
pub fn includes(
    schema: &ValidFederationSchema,
    left: &Valid<ExecutableDocument>,
    right: &Valid<ExecutableDocument>,
) -> Result<(), ComparisonError> {
    includes_with_constraint(schema, &NoConstraint, left, right)
}

/// `includes`, with an extra oracle narrowing each field's possible response types.
///
/// The ported algorithm derives possible types from the schema itself, so no oracle is needed to
/// reproduce the model. This exists for knowledge the model does not have: in a federated context
/// which subgraphs can resolve a field bounds what its response can be, which `SubgraphConstraint`
/// supplies. Pass `NoConstraint` for the model's own behavior.
pub(crate) fn includes_with_constraint<T: PathConstraint>(
    schema: &ValidFederationSchema,
    constraint: &T,
    left: &Valid<ExecutableDocument>,
    right: &Valid<ExecutableDocument>,
) -> Result<(), ComparisonError> {
    QueryComparator::new(schema)?.includes_with_constraint(constraint, left, right)
}

/// A comparator that holds the schema index across comparisons.
///
/// [`includes`] builds a possible-type index over the whole schema before it looks at either
/// operation. Measured on the eight-type schema in the differential harness that setup is about
/// 2.7us of the roughly 4.5us a comparison costs, and it grows with the schema rather than the
/// query, so on a supergraph with hundreds of types it dominates outright. Callers comparing many
/// operation pairs against one schema should build this once and reuse it.
pub struct QueryComparator<'schema> {
    schema: &'schema ValidFederationSchema,
    view: SchemaView<'schema>,
}

impl<'schema> QueryComparator<'schema> {
    pub fn new(schema: &'schema ValidFederationSchema) -> Result<Self, ComparisonError> {
        let view = SchemaView::new(schema).map_err(|e| {
            ComparisonError::internal(format!("failed to index the schema for comparison: {e}"))
        })?;
        Ok(QueryComparator { schema, view })
    }

    /// Does `left` include `right`? See [`includes`].
    pub fn includes(
        &self,
        left: &Valid<ExecutableDocument>,
        right: &Valid<ExecutableDocument>,
    ) -> Result<(), ComparisonError> {
        self.includes_with_constraint(&NoConstraint, left, right)
    }

    /// `includes`, with an extra oracle. See [`includes_with_constraint`].
    pub(crate) fn includes_with_constraint<T: PathConstraint>(
        &self,
        constraint: &T,
        left: &Valid<ExecutableDocument>,
        right: &Valid<ExecutableDocument>,
    ) -> Result<(), ComparisonError> {
        let schema = self.schema;
        let schema_view = &self.view;
        let left_op = single_operation(left)?;
        let right_op = single_operation(right)?;

        let left_root = root_type(schema, left_op)?;
        let right_root = root_type(schema, right_op)?;
        if left_root != right_root {
            return Err(ComparisonError::new(Mismatch::RootTypeMismatch {
                left: left_root,
                right: right_root,
            }));
        }

        compare_shared_variable_declarations(left_op, right_op)?;

        let left_selections: Vec<&Selection> = left_op.selection_set.selections.iter().collect();
        let right_selections: Vec<&Selection> = right_op.selection_set.selections.iter().collect();

        selection_set_includes(
            schema_view,
            constraint,
            &right_root,
            &left_selections,
            &right_selections,
        )
    }
}

fn single_operation(doc: &Valid<ExecutableDocument>) -> Result<&Operation, ComparisonError> {
    doc.operations
        .get(None)
        .map(|op| &**op)
        .map_err(|_| ComparisonError::internal("expected exactly one operation".to_string()))
}

fn root_type(
    schema: &ValidFederationSchema,
    operation: &Operation,
) -> Result<Name, ComparisonError> {
    schema
        .schema()
        .root_operation(operation.operation_type)
        .cloned()
        .ok_or_else(|| {
            ComparisonError::internal(format!(
                "schema has no {} root type",
                operation.operation_type
            ))
        })
}

/// Only variables declared by *both* operations are constrained, and they must agree on declared
/// type and default. Declarations on one side only are free: the semantic relation quantifies
/// over every variable environment, so a variable the other operation never reads cannot change
/// what it returns.
fn compare_shared_variable_declarations(
    left: &Operation,
    right: &Operation,
) -> Result<(), ComparisonError> {
    for left_var in &left.variables {
        let Some(right_var) = right
            .variables
            .iter()
            .find(|candidate| candidate.name == left_var.name)
        else {
            continue;
        };
        let same_default = match (&left_var.default_value, &right_var.default_value) {
            (None, None) => true,
            (Some(left_default), Some(right_default)) => {
                same_argument_value(left_default, right_default)
            }
            _ => false,
        };
        if left_var.ty != right_var.ty || !same_default {
            return Err(ComparisonError::new(
                Mismatch::VariableDeclarationMismatch {
                    variable: left_var.name.clone(),
                    left: error::display_variable_declaration(
                        &left_var.ty,
                        left_var.default_value.as_deref(),
                    ),
                    right: error::display_variable_declaration(
                        &right_var.ty,
                        right_var.default_value.as_deref(),
                    ),
                },
            ));
        }
    }
    Ok(())
}

//==================================================================================================
// Guarded field groups

/// Every occurrence of one response name in a selection-set boundary, each with the condition
/// that gates it. Grouping by response name is what lets the search refine only the conditions
/// that can change *this* group, instead of every condition in the query.
struct GuardedFieldGroup<'doc> {
    response_name: Name,
    entries: Vec<ConditionedField<'doc>>,
}

/// Compiles a flat conditioned-field stream into response-name-local groups in one pass,
/// preserving first-occurrence order of groups and source order within each group.
fn guarded_field_groups<'doc>(
    entries: Vec<ConditionedField<'doc>>,
) -> Vec<GuardedFieldGroup<'doc>> {
    let mut groups: Vec<GuardedFieldGroup<'doc>> = Vec::new();
    for entry in entries {
        let response_name = entry.response_name().clone();
        match groups
            .iter_mut()
            .find(|group| group.response_name == response_name)
        {
            Some(group) => group.entries.push(entry),
            None => groups.push(GuardedFieldGroup {
                response_name,
                entries: vec![entry],
            }),
        }
    }
    groups
}

fn empty_group(response_name: &Name) -> GuardedFieldGroup<'static> {
    GuardedFieldGroup {
        response_name: response_name.clone(),
        entries: Vec::new(),
    }
}

/// Splits `region` into the part `allowed` admits and the part it excludes, dropping empties.
fn split_possible_type_region(region: &[Name], allowed: &[Name]) -> Vec<Vec<Name>> {
    let (included, excluded): (Vec<Name>, Vec<Name>) =
        region.iter().cloned().partition(|ty| allowed.contains(ty));
    let mut result = Vec::new();
    if !included.is_empty() {
        result.push(included);
    }
    if !excluded.is_empty() {
        result.push(excluded);
    }
    result
}

/// Refines the parent's runtime types into regions on which every condition of this response
/// name is constant. Every type in a region is therefore interchangeable, which is why the
/// per-region checks below only ever look at the region's first type.
fn guarded_field_group_type_regions(
    parent_region: &[Name],
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
) -> Vec<Vec<Name>> {
    let mut regions: Vec<Vec<Name>> = if parent_region.is_empty() {
        Vec::new()
    } else {
        vec![parent_region.to_vec()]
    };
    for entry in left.entries.iter().chain(right.entries.iter()) {
        regions = regions
            .iter()
            .flat_map(|region| split_possible_type_region(region, &entry.condition.possible_types))
            .collect();
    }
    regions
}

/// The `@skip`/`@include` variables that can affect this response name, in first-use order.
fn guarded_field_group_boolean_variables(
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
) -> Vec<Name> {
    let mut variables: Vec<Name> = Vec::new();
    for entry in left.entries.iter().chain(right.entries.iter()) {
        for literal in &entry.condition.boolean_condition {
            if !variables.contains(literal.variable()) {
                variables.push(literal.variable().clone());
            }
        }
    }
    variables
}

/// The occurrences that are active for `runtime_type` under `assignment`.
fn active_fields<'doc>(
    group: &GuardedFieldGroup<'doc>,
    assignment: &Assignment,
    runtime_type: &Name,
) -> Vec<&'doc Field> {
    group
        .entries
        .iter()
        .filter(|entry| entry.condition.allows(assignment, runtime_type))
        .map(|entry| entry.field)
        .collect()
}

/// The occurrences whose type condition admits `runtime_type`, regardless of Boolean conditions.
/// The symbolic shortcuts work over these, deciding the Boolean part by clause subtraction
/// instead of by enumeration.
fn entries_at_runtime_type<'a, 'doc>(
    group: &'a GuardedFieldGroup<'doc>,
    runtime_type: &Name,
) -> Vec<&'a ConditionedField<'doc>> {
    group
        .entries
        .iter()
        .filter(|entry| entry.condition.possible_types.contains(runtime_type))
        .collect()
}

//==================================================================================================
// Child obligations

/// One recursive obligation left after the shallow response-name and resolver-call comparison.
struct ChildTask<'doc, T> {
    /// The exact object types the field can return. Using the field's own return rather than the
    /// declared interface is what keeps a covariant return from being widened to every
    /// implementation of that interface.
    possible_types: Vec<Name>,
    left_selections: Vec<&'doc Selection>,
    right_selections: Vec<&'doc Selection>,
    /// The oracle for the field's sub-selection scope.
    constraint: T,
}

/// Matches the right side's active fields against the left's for one concrete parent type.
///
/// `Ok(None)` closes the obligation here: either the right side selects nothing, or the field is
/// a leaf whose value is already forced by the matching resolver call. `Ok(Some(task))` defers to
/// the sub-selection. An `Err` is where the Lean `matchInclusionChildTask?` would return a bare
/// `none`; the four cases are kept apart because "left never selects this" and "left passes
/// different arguments" call for different fixes.
fn match_child_task<'doc, T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    parent_type: &Name,
    response_name: &Name,
    runtime_type: &Name,
    left_fields: &[&'doc Field],
    right_fields: &[&'doc Field],
) -> Result<Option<ChildTask<'doc, T>>, Mismatch> {
    let Some(right_first) = right_fields.first() else {
        return Ok(None);
    };
    let Some(left_first) = left_fields.first() else {
        return Err(Mismatch::MissingResponseName {
            response_name: response_name.clone(),
            field_name: right_first.name.clone(),
            runtime_type: runtime_type.clone(),
        });
    };
    if left_first.name != right_first.name {
        return Err(Mismatch::FieldNameMismatch {
            response_name: response_name.clone(),
            left: left_first.name.clone(),
            right: right_first.name.clone(),
        });
    }
    if !same_arguments(&left_first.arguments, &right_first.arguments) {
        return Err(Mismatch::FieldArgumentsMismatch {
            response_name: response_name.clone(),
            field_name: right_first.name.clone(),
            left: error::render_arguments(&left_first.arguments),
            right: error::render_arguments(&right_first.arguments),
        });
    }
    let Some(definition) = schema.lookup_field(parent_type, &right_first.name) else {
        return Err(Mismatch::UndefinedField {
            parent_type: parent_type.clone(),
            field_name: right_first.name.clone(),
        });
    };
    if !schema.is_composite(&definition.ty) {
        return Ok(None);
    }
    // The schema says what the field's return type admits; the oracle may know it is narrower
    // still. `NoConstraint` answers `All`, leaving the schema's answer untouched.
    let parent_types = PossibleTypes::Restricted(
        std::iter::once(ObjectTypeDefinitionPosition::new(parent_type.clone())).collect(),
    );
    let (child_constraint, allowed) =
        constraint
            .for_field(right_first, &parent_types)
            .map_err(|e| Mismatch::Internal {
                message: format!("path constraint failed for {}: {e}", right_first.name),
            })?;
    Ok(Some(ChildTask {
        possible_types: schema
            .possible_types(definition.ty.inner_named_type())
            .iter()
            .filter(|name| allowed.allows(&ObjectTypeDefinitionPosition::new((*name).clone())))
            .cloned()
            .collect(),
        left_selections: merged_selections(left_fields),
        right_selections: merged_selections(right_fields),
        constraint: child_constraint,
    }))
}

/// The sub-selections of every occurrence of one response name, concatenated. GraphQL merges
/// fields by response name, so all of them contribute to the same response object.
fn merged_selections<'doc>(fields: &[&'doc Field]) -> Vec<&'doc Selection> {
    fields
        .iter()
        .flat_map(|field| field.selection_set.selections.iter())
        .collect()
}

/// Computes child obligations for every concrete parent type in the region, de-duplicating the
/// ones that coincide. Covariant returns that differ only by parent type keep separate
/// obligations, because their return regions differ.
fn child_tasks_for_parent_types<'doc, T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    parent_types: &[Name],
    response_name: &Name,
    runtime_type: &Name,
    left_fields: &[&'doc Field],
    right_fields: &[&'doc Field],
) -> Result<Vec<ChildTask<'doc, T>>, Mismatch> {
    let mut tasks: Vec<ChildTask<'doc, T>> = Vec::new();
    for parent_type in parent_types {
        let Some(task) = match_child_task(
            schema,
            constraint,
            parent_type,
            response_name,
            runtime_type,
            left_fields,
            right_fields,
        )?
        else {
            continue;
        };
        // Tasks from different parent types share the same selection slices, so identity on the
        // return region plus pointer identity on the selections is enough to spot duplicates.
        let duplicate = tasks.iter().any(|existing| {
            existing.possible_types == task.possible_types
                && same_selection_refs(&existing.left_selections, &task.left_selections)
                && same_selection_refs(&existing.right_selections, &task.right_selections)
        });
        if !duplicate {
            tasks.push(task);
        }
    }
    Ok(tasks)
}

fn same_selection_refs(left: &[&Selection], right: &[&Selection]) -> bool {
    left.len() == right.len()
        && std::iter::zip(left, right).all(|(left, right)| std::ptr::eq(*left, *right))
}

//==================================================================================================
// The search

fn selection_set_includes<T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    parent_type: &Name,
    left_selections: &[&Selection],
    right_selections: &[&Selection],
) -> Result<(), ComparisonError> {
    let left_entries = of_selection_set(schema, parent_type, left_selections)?;
    let right_entries = of_selection_set(schema, parent_type, right_selections)?;
    guarded_field_groups_include(
        schema,
        constraint,
        Some(parent_type),
        &Assignment::default(),
        &guarded_field_groups(left_entries),
        &guarded_field_groups(right_entries),
    )
}

/// Inclusion is the conjunction of the obligations for the response names the right operation
/// can produce. A response name the left operation never selects is represented by an empty
/// group, so it fails exactly when the right group turns out to be feasible.
fn guarded_field_groups_include<T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    fixed_parent_type: Option<&Name>,
    assignment: &Assignment,
    left_groups: &[GuardedFieldGroup<'_>],
    right_groups: &[GuardedFieldGroup<'_>],
) -> Result<(), ComparisonError> {
    for right in right_groups {
        let fallback = empty_group(&right.response_name);
        let left = left_groups
            .iter()
            .find(|group| group.response_name == right.response_name)
            .unwrap_or(&fallback);

        let parent_region: Vec<Name> = match fixed_parent_type {
            Some(parent_type) => schema.possible_types(parent_type).to_vec(),
            None => {
                // Without a fixed execution parent type the region is whatever the conditions
                // themselves reach.
                let mut region: Vec<Name> = Vec::new();
                for entry in left.entries.iter().chain(right.entries.iter()) {
                    for ty in &entry.condition.possible_types {
                        if !region.contains(ty) {
                            region.push(ty.clone());
                        }
                    }
                }
                region
            }
        };
        let regions = guarded_field_group_type_regions(&parent_region, left, right);

        // The local shortcuts are witnesses, not obligations: when one succeeds the group is
        // settled, and when none does the general search produces the explanation.
        if group_locally_includes(schema, fixed_parent_type, left, right, &regions) {
            continue;
        }
        let variables = guarded_field_group_boolean_variables(left, right);
        group_includes_under_assignments(
            schema,
            constraint,
            fixed_parent_type,
            assignment,
            &variables,
            left,
            right,
            &regions,
        )
        .map_err(|e| {
            e.add_context(PathSegment::ResponseName {
                name: right.response_name.clone(),
            })
        })?;
    }
    Ok(())
}

/// Enumerates the Boolean assignments that can change this group, then checks the type regions
/// under each complete assignment. Variables already bound by an enclosing scope are not re-split.
#[allow(clippy::too_many_arguments)]
fn group_includes_under_assignments<T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    fixed_parent_type: Option<&Name>,
    assignment: &Assignment,
    remaining_variables: &[Name],
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
    regions: &[Vec<Name>],
) -> Result<(), ComparisonError> {
    let Some((variable, rest)) = remaining_variables.split_first() else {
        return group_includes_for_regions(
            schema,
            constraint,
            fixed_parent_type,
            assignment,
            left,
            right,
            regions,
        );
    };
    if assignment.get(variable).is_some() {
        return group_includes_under_assignments(
            schema,
            constraint,
            fixed_parent_type,
            assignment,
            rest,
            left,
            right,
            regions,
        );
    }
    for value in [false, true] {
        let extended = assignment.extended(variable.clone(), value);
        group_includes_under_assignments(
            schema,
            constraint,
            fixed_parent_type,
            &extended,
            rest,
            left,
            right,
            regions,
        )
        .map_err(|e| {
            e.add_context(PathSegment::BooleanAssignment {
                assignment: Assignment::default().extended(variable.clone(), value),
            })
        })?;
    }
    Ok(())
}

/// Checks one response name for a fixed assignment, over every relevant type region.
#[allow(clippy::too_many_arguments)]
fn group_includes_for_regions<T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    fixed_parent_type: Option<&Name>,
    assignment: &Assignment,
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
    regions: &[Vec<Name>],
) -> Result<(), ComparisonError> {
    for region in regions {
        // Every type in a region satisfies exactly the same conditions, so the first one stands
        // for all of them when deciding which occurrences are active.
        let Some(runtime_type) = region.first() else {
            continue;
        };
        let left_fields = active_fields(left, assignment, runtime_type);
        let right_fields = active_fields(right, assignment, runtime_type);

        let parent_types: Vec<Name> = match fixed_parent_type {
            Some(parent_type) => vec![parent_type.clone()],
            None => region.clone(),
        };
        let region_context = || PathSegment::TypeRegion {
            types: region.clone(),
        };
        let tasks = child_tasks_for_parent_types(
            schema,
            constraint,
            &parent_types,
            &right.response_name,
            runtime_type,
            &left_fields,
            &right_fields,
        )
        .map_err(|reason| ComparisonError::new(reason).add_context(region_context()))?;

        for task in tasks {
            if selection_set_syntactically_includes(&task.left_selections, &task.right_selections) {
                continue;
            }
            let left_child = of_type_region(schema, &task.possible_types, &task.left_selections)?;
            let right_child = of_type_region(schema, &task.possible_types, &task.right_selections)?;
            guarded_field_groups_include(
                schema,
                &task.constraint,
                None,
                assignment,
                &guarded_field_groups(left_child),
                &guarded_field_groups(right_child),
            )
            .map_err(|e| {
                e.add_context(PathSegment::ChildSelection {
                    field: right_fields
                        .first()
                        .map(|field| field.name.clone())
                        .unwrap_or_else(|| right.response_name.clone()),
                    possible_types: task.possible_types.clone(),
                })
                .add_context(region_context())
            })?;
        }
    }
    Ok(())
}

//==================================================================================================
// Local shortcuts
//
// Both decide a response name without enumerating Boolean assignments. They are witnesses: a
// `false` here only means "no cheap proof", and the general search still runs.

fn group_locally_includes(
    schema: &SchemaView<'_>,
    fixed_parent_type: Option<&Name>,
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
    regions: &[Vec<Name>],
) -> bool {
    scalar_field_includes(schema, fixed_parent_type, left, right, regions)
        || composite_field_includes(schema, fixed_parent_type, left, right, regions)
}

fn scalar_field_includes(
    schema: &SchemaView<'_>,
    fixed_parent_type: Option<&Name>,
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
    regions: &[Vec<Name>],
) -> bool {
    regions.iter().flatten().all(|runtime_type| {
        scalar_field_includes_at_runtime_type(schema, fixed_parent_type, runtime_type, left, right)
    })
}

/// Decides a scalar field group symbolically. Requiring one resolver call across the whole
/// runtime-type slice matches the field-merge invariant directly, while clause subtraction
/// handles unions such as `@include(if: $x)` together with `@skip(if: $x)`.
fn scalar_field_includes_at_runtime_type(
    schema: &SchemaView<'_>,
    fixed_parent_type: Option<&Name>,
    runtime_type: &Name,
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
) -> bool {
    if left.response_name != right.response_name {
        return false;
    }
    let left_entries = entries_at_runtime_type(left, runtime_type);
    let right_entries = entries_at_runtime_type(right, runtime_type);
    let Some(right_head) = right_entries.first() else {
        return true;
    };
    let parent_type = fixed_parent_type.unwrap_or(runtime_type);
    let Some(definition) = schema.lookup_field(parent_type, &right_head.field.name) else {
        return false;
    };
    if schema.is_composite(&definition.ty) {
        return false;
    }
    let same_call =
        |entry: &&ConditionedField<'_>| same_resolver_call(entry.field, right_head.field);
    if !left_entries.iter().all(same_call) || !right_entries.iter().all(same_call) {
        return false;
    }
    let left_clauses: Vec<BooleanCondition> = left_entries
        .iter()
        .map(|entry| entry.condition.boolean_condition.clone())
        .collect();
    right_entries.iter().all(|entry| {
        boolean_condition_covered_by(&entry.condition.boolean_condition, &left_clauses)
    })
}

fn composite_field_includes(
    schema: &SchemaView<'_>,
    fixed_parent_type: Option<&Name>,
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
    regions: &[Vec<Name>],
) -> bool {
    regions.iter().flatten().all(|runtime_type| {
        composite_field_includes_at_runtime_type(
            schema,
            fixed_parent_type,
            runtime_type,
            left,
            right,
        )
    })
}

/// The composite counterpart of the scalar shortcut, deliberately limited to the one-to-one case:
/// field merging stays with the general search, while a broader type or directive guard can still
/// bypass recursion when the child selections have a syntactic witness.
fn composite_field_includes_at_runtime_type(
    schema: &SchemaView<'_>,
    fixed_parent_type: Option<&Name>,
    runtime_type: &Name,
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
) -> bool {
    if left.response_name != right.response_name {
        return false;
    }
    let left_entries = entries_at_runtime_type(left, runtime_type);
    let right_entries = entries_at_runtime_type(right, runtime_type);
    let [right_entry] = right_entries.as_slice() else {
        return right_entries.is_empty();
    };
    let [left_entry] = left_entries.as_slice() else {
        return false;
    };
    if !same_resolver_call(left_entry.field, right_entry.field) {
        return false;
    }
    if !boolean_condition_covered_by(
        &right_entry.condition.boolean_condition,
        std::slice::from_ref(&left_entry.condition.boolean_condition),
    ) {
        return false;
    }
    let parent_type = fixed_parent_type.unwrap_or(runtime_type);
    let Some(definition) = schema.lookup_field(parent_type, &right_entry.field.name) else {
        return false;
    };
    if !schema.is_composite(&definition.ty) {
        return false;
    }
    let left_selections: Vec<&Selection> =
        left_entry.field.selection_set.selections.iter().collect();
    let right_selections: Vec<&Selection> =
        right_entry.field.selection_set.selections.iter().collect();
    selection_set_syntactically_includes(&left_selections, &right_selections)
}

//==================================================================================================
// Syntactic inclusion shortcut

/// Selection order and extra left selections do not affect inclusion. This check stays
/// syntax-only: condition normalization and field merging remain the general search's job.
fn selection_set_syntactically_includes(left: &[&Selection], right: &[&Selection]) -> bool {
    right.iter().all(|right_selection| {
        left.iter()
            .any(|left_selection| selection_syntactically_includes(left_selection, right_selection))
    })
}

fn selection_syntactically_includes(left: &Selection, right: &Selection) -> bool {
    match (left, right) {
        (Selection::Field(left), Selection::Field(right)) => {
            left.response_key() == right.response_key()
                && left.name == right.name
                && same_arguments(&left.arguments, &right.arguments)
                && same_directive_list(&left.directives, &right.directives)
                && selection_set_syntactically_includes(
                    &left.selection_set.selections.iter().collect::<Vec<_>>(),
                    &right.selection_set.selections.iter().collect::<Vec<_>>(),
                )
        }
        (Selection::InlineFragment(left), Selection::InlineFragment(right)) => {
            left.type_condition == right.type_condition
                && same_directive_list(&left.directives, &right.directives)
                && selection_set_syntactically_includes(
                    &left.selection_set.selections.iter().collect::<Vec<_>>(),
                    &right.selection_set.selections.iter().collect::<Vec<_>>(),
                )
        }
        _ => false,
    }
}

//==================================================================================================
// Syntactic comparison of resolver calls

/// Do these two field occurrences denote the same resolver call? Argument order is immaterial in
/// GraphQL, so arguments are compared as a set keyed by name.
fn same_resolver_call(left: &Field, right: &Field) -> bool {
    left.name == right.name && same_arguments(&left.arguments, &right.arguments)
}

fn same_arguments(left: &[Node<ast::Argument>], right: &[Node<ast::Argument>]) -> bool {
    left.len() == right.len()
        && left.iter().all(|left_argument| {
            right.iter().any(|right_argument| {
                left_argument.name == right_argument.name
                    && same_argument_value(&left_argument.value, &right_argument.value)
            })
        })
}

/// Input-object field order is immaterial; list order is not.
fn same_argument_value(left: &ast::Value, right: &ast::Value) -> bool {
    match (left, right) {
        (ast::Value::Object(left), ast::Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(left_name, left_value)| {
                    right.iter().any(|(right_name, right_value)| {
                        left_name == right_name && same_argument_value(left_value, right_value)
                    })
                })
        }
        (ast::Value::List(left), ast::Value::List(right)) => {
            left.len() == right.len()
                && std::iter::zip(left, right).all(|(left, right)| same_argument_value(left, right))
        }
        _ => left == right,
    }
}

/// Directive lists are compared in order, matching the model's `directiveListEqBool`. Only
/// `@skip` and `@include` can reach here; anything else is rejected during extraction.
fn same_directive_list(left: &ast::DirectiveList, right: &ast::DirectiveList) -> bool {
    left.len() == right.len()
        && std::iter::zip(left.iter(), right.iter()).all(|(left, right)| {
            left.name == right.name && same_arguments(&left.arguments, &right.arguments)
        })
}
