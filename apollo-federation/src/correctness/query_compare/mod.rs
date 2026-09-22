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
//! Three witnesses bypass the search when a cheaper proof exists: a symbolic clause-subtraction
//! check for scalar fields, a syntactic inclusion check for one-to-one composite fields, and a
//! recursive symbolic check that carries each occurrence's guard into the child boundary instead
//! of case-splitting on it here. All three are present in the Lean source and are ported rather
//! than simplified away, since a divergence in either direction would be a divergence from the
//! model. Each is a witness only: declining one costs nothing but the general search.
//!
//! Scope: the model gives semantics to `@skip` and `@include` only. Any other directive is carried
//! as part of a field's resolver call and compared, but is otherwise uninterpreted, so two fields
//! differing only by one are not treated as the same call. Named fragment spreads are resolved
//! where they are reached, symmetrically with inline fragments — see `conditions::fragment_view`.
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
//! step descends into strictly less remaining syntax of the right operation. A fragment spread is
//! the one step that does not descend into a sub-term, since it resolves to a definition
//! elsewhere in the document; that still terminates because a valid document has no fragment
//! reference cycles, which GraphQL validation guarantees and this checker requires of its inputs.
//!
//! **Specialized inherited condition.** `extractFields` threads an `inheritedBooleanCondition`
//! that both entry points pass as empty; see `conditions.rs`.
//!
//! **When the symbolic witness is attempted.** The model tries
//! `guardedFieldGroupSymbolicallyIncludesWithFuel` for any group that mentions a Boolean variable
//! at all; `group_symbolically_includes` is tried only when some such variable is still unbound,
//! since with all of them bound the general search does no splitting and keeps its own shortcuts.
//! That is strictly fewer attempts, and a witness not attempted decides nothing.
//!
//! **Region representative in the symbolic witness.** The model's rule runs over every runtime
//! type of a region; this port takes the region's first, as both do in the general search. A
//! region is refined until every one of its types satisfies the same conditions, and the runtime
//! type feeds nothing but the entry filter, so the two agree.
//!
//! **Canonical region order.** `getPossibleTypes` returns schema declaration order; this port
//! sorts by name. Regions are sets, so the verdict is unaffected, and sorting makes the
//! child-task de-duplication in `child_tasks_for_parent_types` canonical.

pub(crate) mod conditions;
mod error;
mod schema_view;

#[cfg(test)]
mod tests;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::validation::Valid;

pub use self::conditions::Assignment;
use self::conditions::BooleanCondition;
use self::conditions::ConditionedField;
use self::conditions::FragmentView;
use self::conditions::boolean_condition_covered_by;
use self::conditions::fragment_view;
use self::conditions::of_selection_set;
use self::conditions::of_type_region;
use self::conditions::of_type_region_under;
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
            Scope {
                fragments: &left.fragments,
            },
            &right_selections,
            Scope {
                fragments: &right.fragments,
            },
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
    /// The document these entries came from. Carried on the group rather than threaded through
    /// the search, because a group always belongs to exactly one side.
    scope: Scope<'doc>,
}

/// Selections together with the fragment definitions they may spread.
///
/// A spread is only meaningful next to the document that defines it, and the two operations being
/// compared have different documents, so selections never travel bare.
#[derive(Clone, Copy)]
struct Scope<'doc> {
    fragments: &'doc FragmentMap,
}

/// Compiles a flat conditioned-field stream into response-name-local groups in one pass,
/// preserving first-occurrence order of groups and source order within each group.
fn guarded_field_groups<'doc>(
    scope: Scope<'doc>,
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
                scope,
            }),
        }
    }
    groups
}

fn empty_group<'doc>(response_name: &Name, scope: Scope<'doc>) -> GuardedFieldGroup<'doc> {
    GuardedFieldGroup {
        response_name: response_name.clone(),
        entries: Vec::new(),
        scope,
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
    left_scope: Scope<'doc>,
    right_scope: Scope<'doc>,
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
#[allow(clippy::too_many_arguments)]
fn match_child_task<'doc, T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    parent_type: &Name,
    response_name: &Name,
    runtime_type: &Name,
    left_fields: &[&'doc Field],
    left_scope: Scope<'doc>,
    right_fields: &[&'doc Field],
    right_scope: Scope<'doc>,
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
    // Directives the model gives no semantics to are still part of the resolver call: two fields
    // differing only by a custom directive are not the same call. `@skip`/`@include` never reach
    // here, having been absorbed into the conditions.
    let left_directives = unmodeled_directives(&left_first.directives);
    let right_directives = unmodeled_directives(&right_first.directives);
    if !same_directives(&left_directives, &right_directives) {
        return Err(Mismatch::FieldDirectivesMismatch {
            response_name: response_name.clone(),
            field_name: right_first.name.clone(),
            left: render_directives(&left_directives),
            right: render_directives(&right_directives),
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
        left_scope,
        right_selections: merged_selections(right_fields),
        right_scope,
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
#[allow(clippy::too_many_arguments)]
fn child_tasks_for_parent_types<'doc, T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    parent_types: &[Name],
    response_name: &Name,
    runtime_type: &Name,
    left_fields: &[&'doc Field],
    left_scope: Scope<'doc>,
    right_fields: &[&'doc Field],
    right_scope: Scope<'doc>,
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
            left_scope,
            right_fields,
            right_scope,
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

fn selection_set_includes<'doc, T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    parent_type: &Name,
    left_selections: &[&'doc Selection],
    left_scope: Scope<'doc>,
    right_selections: &[&'doc Selection],
    right_scope: Scope<'doc>,
) -> Result<(), ComparisonError> {
    let left_entries =
        of_selection_set(schema, left_scope.fragments, parent_type, left_selections)?;
    let right_entries =
        of_selection_set(schema, right_scope.fragments, parent_type, right_selections)?;
    guarded_field_groups_include(
        schema,
        constraint,
        Some(parent_type),
        &Assignment::default(),
        &guarded_field_groups(left_scope, left_entries),
        &guarded_field_groups(right_scope, right_entries),
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
        let fallback = empty_group(&right.response_name, right.scope);
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
        // Only worth attempting where the search would actually branch; with every variable
        // already bound the enumeration below does no splitting and keeps its own shortcuts.
        let would_split = variables
            .iter()
            .any(|variable| assignment.get(variable).is_none());
        if would_split
            && group_symbolically_includes(
                schema,
                constraint,
                fixed_parent_type,
                assignment,
                left,
                right,
                &regions,
            )
        {
            continue;
        }
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

//==================================================================================================
// Symbolic descent
//
// The enumeration below binds every variable a response name mentions before it looks at the
// sub-selection, so two guarded subtrees that share nothing but a parent still cost the product
// of their assignments. Deciding the guard symbolically and re-deriving it one boundary down
// turns that product back into a sum, which is the difference between 2^13 and 2^7 + 2^6 on a
// query whose root field carries every fetch of the plan.

/// Decides one response name without case-splitting, by carrying each occurrence's Boolean
/// condition into the child comparison instead of binding it here.
///
/// Applicable at a runtime type when every occurrence of the response name, on both sides, makes
/// the same resolver call, and the left side's clauses cover each right clause — the same two
/// facts `scalar_field_includes` establishes, decided the same symbolic way, but now also usable
/// for a composite field because the child comparison re-derives the guards it needs. A variable
/// is then split only at the depth that reads it.
///
/// Returns whether inclusion was *proved*. A `false` is "no proof by this route" and never a
/// verdict: the caller runs the general search, which owns both the answer and its explanation.
#[allow(clippy::too_many_arguments)]
fn group_symbolically_includes<T: PathConstraint>(
    schema: &SchemaView<'_>,
    constraint: &T,
    fixed_parent_type: Option<&Name>,
    assignment: &Assignment,
    left: &GuardedFieldGroup<'_>,
    right: &GuardedFieldGroup<'_>,
    regions: &[Vec<Name>],
) -> bool {
    if left.response_name != right.response_name {
        return false;
    }
    for region in regions {
        // Every type in a region satisfies the same conditions, so the first one stands for all.
        let Some(runtime_type) = region.first() else {
            continue;
        };
        let left_entries = entries_at_runtime_type(left, runtime_type);
        let right_entries = entries_at_runtime_type(right, runtime_type);
        let Some(right_head) = right_entries.first() else {
            continue; // the right side never produces this response name here
        };

        // One resolver call across the whole slice, so that whichever occurrences an assignment
        // makes active, they agree on the call and on what the child boundary is.
        let same_call =
            |entry: &&ConditionedField<'_>| same_resolver_call(entry.field, right_head.field);
        if !left_entries.iter().all(same_call) || !right_entries.iter().all(same_call) {
            return false;
        }

        // Some left occurrence is active whenever a right one is.
        let left_clauses: Vec<BooleanCondition> = left_entries
            .iter()
            .map(|entry| entry.condition.boolean_condition.clone())
            .collect();
        if !right_entries.iter().all(|entry| {
            boolean_condition_covered_by(&entry.condition.boolean_condition, &left_clauses)
        }) {
            return false;
        }

        let left_fields: Vec<&Field> = left_entries.iter().map(|entry| entry.field).collect();
        let right_fields: Vec<&Field> = right_entries.iter().map(|entry| entry.field).collect();
        let parent_types: Vec<Name> = match fixed_parent_type {
            Some(parent_type) => vec![parent_type.clone()],
            None => region.clone(),
        };
        // The model asks for a composite field here and declines otherwise, leaving a scalar one
        // to `scalar_field_includes` — which decides it on the two facts just established, so
        // nothing is lost by matching that.
        let composite_everywhere = parent_types.iter().all(|parent_type| {
            schema
                .lookup_field(parent_type, &right_head.field.name)
                .is_some_and(|definition| schema.is_composite(&definition.ty))
        });
        if !composite_everywhere {
            return false;
        }
        let Ok(tasks) = child_tasks_for_parent_types(
            schema,
            constraint,
            &parent_types,
            &right.response_name,
            runtime_type,
            &left_fields,
            left.scope,
            &right_fields,
            right.scope,
        ) else {
            return false;
        };

        let left_contributions = conditioned_contributions(&left_entries);
        let right_contributions = conditioned_contributions(&right_entries);
        for task in tasks {
            let (Ok(left_child), Ok(right_child)) = (
                of_type_region_under(
                    schema,
                    task.left_scope.fragments,
                    &task.possible_types,
                    &left_contributions,
                ),
                of_type_region_under(
                    schema,
                    task.right_scope.fragments,
                    &task.possible_types,
                    &right_contributions,
                ),
            ) else {
                return false;
            };
            if guarded_field_groups_include(
                schema,
                &task.constraint,
                None,
                assignment,
                &guarded_field_groups(task.left_scope, left_child),
                &guarded_field_groups(task.right_scope, right_child),
            )
            .is_err()
            {
                return false;
            }
        }
    }
    true
}

/// Each occurrence's sub-selections, paired with the condition that gates the occurrence.
fn conditioned_contributions<'doc>(
    entries: &[&ConditionedField<'doc>],
) -> Vec<(BooleanCondition, Vec<&'doc Selection>)> {
    entries
        .iter()
        .map(|entry| {
            (
                entry.condition.boolean_condition.clone(),
                entry.field.selection_set.selections.iter().collect(),
            )
        })
        .collect()
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
            left.scope,
            &right_fields,
            right.scope,
        )
        .map_err(|reason| ComparisonError::new(reason).add_context(region_context()))?;

        for task in tasks {
            if selection_set_syntactically_includes(
                schema,
                &task.possible_types,
                &task.left_selections,
                task.left_scope,
                &task.right_selections,
                task.right_scope,
            ) {
                continue;
            }
            let left_child = of_type_region(
                schema,
                task.left_scope.fragments,
                &task.possible_types,
                &task.left_selections,
            )?;
            let right_child = of_type_region(
                schema,
                task.right_scope.fragments,
                &task.possible_types,
                &task.right_selections,
            )?;
            guarded_field_groups_include(
                schema,
                &task.constraint,
                None,
                assignment,
                &guarded_field_groups(task.left_scope, left_child),
                &guarded_field_groups(task.right_scope, right_child),
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
    selection_set_syntactically_includes(
        schema,
        schema.possible_types(definition.ty.inner_named_type()),
        &left_selections,
        left.scope,
        &right_selections,
        right.scope,
    )
}

//==================================================================================================
// Syntactic inclusion shortcut

/// Is this fragment's type condition doing nothing where it sits?
///
/// A condition admitting every object type the position can hold selects exactly what its body
/// selects, so the two are the same selection written two ways. Recognizing that is what keeps
/// `{ ...F }` and the body of `F` comparable; a query plan writes the first where the operation
/// it is checked against writes the second, and without this the shortcut below fails on every
/// such pair. A directive is uninterpreted here and so is never nothing, and an unknown position
/// (empty) makes no claim.
fn fragment_narrows_nothing(
    schema: &SchemaView<'_>,
    position: &[Name],
    view: &FragmentView<'_>,
) -> bool {
    if !view.directives.is_empty() {
        return false;
    }
    match view.type_condition {
        None => true,
        Some(type_condition) => {
            let admitted = schema.possible_types(type_condition);
            !position.is_empty() && position.iter().all(|name| admitted.contains(name))
        }
    }
}

/// A selection set with every fragment that narrows nothing at `position` replaced by its body,
/// so that the same selections compare equal however they are packaged.
///
/// Terminates for the same reason the rest of this module does: a valid document has no fragment
/// reference cycles.
fn without_transparent_fragments<'doc>(
    schema: &SchemaView<'_>,
    position: &[Name],
    selections: &[&'doc Selection],
    scope: Scope<'doc>,
    out: &mut Vec<&'doc Selection>,
) {
    for selection in selections {
        match fragment_view(selection, scope.fragments) {
            Ok(Some(view)) if fragment_narrows_nothing(schema, position, &view) => {
                let nested: Vec<&Selection> = view.selections.iter().collect();
                without_transparent_fragments(schema, position, &nested, scope, out);
            }
            _ => out.push(selection),
        }
    }
}

/// The object types a field's sub-selection is written against. Empty when the schema does not
/// place the field at every type of `position`, which makes no claim about any condition there.
fn child_position(schema: &SchemaView<'_>, position: &[Name], field_name: &Name) -> Vec<Name> {
    let mut child: Vec<Name> = Vec::new();
    for parent in position {
        let Some(definition) = schema.lookup_field(parent, field_name) else {
            return Vec::new();
        };
        for name in schema.possible_types(definition.ty.inner_named_type()) {
            if !child.contains(name) {
                child.push(name.clone());
            }
        }
    }
    child.sort();
    child
}

/// `position` narrowed by a type condition written at it.
fn narrowed_position(
    schema: &SchemaView<'_>,
    position: &[Name],
    type_condition: Option<&Name>,
) -> Vec<Name> {
    let Some(type_condition) = type_condition else {
        return position.to_vec();
    };
    let admitted = schema.possible_types(type_condition);
    position
        .iter()
        .filter(|name| admitted.contains(name))
        .cloned()
        .collect()
}

/// Is there anything here a position could say something about? Only a fragment can be vacuous,
/// so a set of plain fields needs neither flattening nor a position to compare -- and working one
/// out costs a schema lookup per type the parent can hold.
fn holds_a_fragment(selections: &[&Selection]) -> bool {
    selections
        .iter()
        .any(|selection| !matches!(selection, Selection::Field(_)))
}

/// Selection order and extra left selections do not affect inclusion. This check stays
/// syntax-only, apart from asking the schema which type conditions are vacuous where they sit:
/// condition normalization and field merging remain the general search's job.
fn selection_set_syntactically_includes(
    schema: &SchemaView<'_>,
    position: &[Name],
    left: &[&Selection],
    left_scope: Scope<'_>,
    right: &[&Selection],
    right_scope: Scope<'_>,
) -> bool {
    let matches = |left: &[&Selection], right: &[&Selection]| {
        right.iter().all(|right_selection| {
            left.iter().any(|left_selection| {
                selection_syntactically_includes(
                    schema,
                    position,
                    left_selection,
                    left_scope,
                    right_selection,
                    right_scope,
                )
            })
        })
    };
    if !holds_a_fragment(left) && !holds_a_fragment(right) {
        return matches(left, right);
    }
    let mut left_flat = Vec::with_capacity(left.len());
    without_transparent_fragments(schema, position, left, left_scope, &mut left_flat);
    let mut right_flat = Vec::with_capacity(right.len());
    without_transparent_fragments(schema, position, right, right_scope, &mut right_flat);
    matches(&left_flat, &right_flat)
}

fn selection_syntactically_includes(
    schema: &SchemaView<'_>,
    position: &[Name],
    left: &Selection,
    left_scope: Scope<'_>,
    right: &Selection,
    right_scope: Scope<'_>,
) -> bool {
    // A spread and an inline fragment with the same condition, directives and selections are one
    // fragment written two ways, so each side resolves to a view before anything is compared.
    let (Ok(left_view), Ok(right_view)) = (
        fragment_view(left, left_scope.fragments),
        fragment_view(right, right_scope.fragments),
    ) else {
        return false;
    };
    match (left_view, right_view) {
        (Some(left_view), Some(right_view)) => {
            left_view.type_condition == right_view.type_condition
                && same_directive_list(left_view.directives, right_view.directives)
                && selection_set_syntactically_includes(
                    schema,
                    // Both sides carry the same condition, so the body sits where it narrows to.
                    &narrowed_position(schema, position, left_view.type_condition),
                    &left_view.selections.iter().collect::<Vec<_>>(),
                    left_scope,
                    &right_view.selections.iter().collect::<Vec<_>>(),
                    right_scope,
                )
        }
        (None, None) => {
            let (Selection::Field(left), Selection::Field(right)) = (left, right) else {
                return false;
            };
            left.response_key() == right.response_key()
                && left.name == right.name
                && same_arguments(&left.arguments, &right.arguments)
                && same_directive_list(&left.directives, &right.directives)
                && {
                    let left_sub: Vec<&Selection> = left.selection_set.selections.iter().collect();
                    let right_sub: Vec<&Selection> =
                        right.selection_set.selections.iter().collect();
                    // Only worth locating the sub-selection when something there could be vacuous.
                    let child = if holds_a_fragment(&left_sub) || holds_a_fragment(&right_sub) {
                        child_position(schema, position, &right.name)
                    } else {
                        Vec::new()
                    };
                    selection_set_syntactically_includes(
                        schema,
                        &child,
                        &left_sub,
                        left_scope,
                        &right_sub,
                        right_scope,
                    )
                }
        }
        _ => false,
    }
}

//==================================================================================================
// Syntactic comparison of resolver calls

/// Do these two field occurrences denote the same resolver call?
///
/// Argument order is immaterial in GraphQL, so arguments are compared as a set keyed by name, and
/// so are the directives the model gives no semantics to. `@skip`/`@include` are absorbed into
/// conditions and deliberately excluded here: two occurrences of a field under different Boolean
/// conditions are the same call, made under different circumstances.
fn same_resolver_call(left: &Field, right: &Field) -> bool {
    left.name == right.name
        && same_arguments(&left.arguments, &right.arguments)
        && same_directives(
            &unmodeled_directives(&left.directives),
            &unmodeled_directives(&right.directives),
        )
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

/// Directive lists are compared in order, matching the model's `directiveListEqBool`.
fn same_directive_list(left: &ast::DirectiveList, right: &ast::DirectiveList) -> bool {
    left.len() == right.len()
        && std::iter::zip(left.iter(), right.iter()).all(|(left, right)| {
            left.name == right.name && same_arguments(&left.arguments, &right.arguments)
        })
}

/// The directives that carry no modeled semantics: everything but `@skip` and `@include`, which
/// are absorbed into a selection's condition instead.
fn unmodeled_directives(directives: &ast::DirectiveList) -> Vec<&Node<ast::Directive>> {
    directives
        .iter()
        .filter(|directive| directive.name != "skip" && directive.name != "include")
        .collect()
}

/// Compared as a set by name, like arguments: directive order is not semantically meaningful.
fn same_directives(left: &[&Node<ast::Directive>], right: &[&Node<ast::Directive>]) -> bool {
    left.len() == right.len()
        && left.iter().all(|left_directive| {
            right.iter().any(|right_directive| {
                left_directive.name == right_directive.name
                    && same_arguments(&left_directive.arguments, &right_directive.arguments)
            })
        })
}

fn render_directives(directives: &[&Node<ast::Directive>]) -> String {
    if directives.is_empty() {
        return "(none)".to_string();
    }
    directives
        .iter()
        .map(|directive| format!("@{}", directive.name))
        .collect::<Vec<_>>()
        .join(" ")
}
