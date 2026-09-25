pub mod query_compare;
pub mod query_plan_analysis;
#[cfg(test)]
pub mod query_plan_analysis_test;
pub mod query_plan_check;
mod query_plan_soundness;
#[cfg(test)]
pub mod query_plan_soundness_test;
pub mod response_shape;
pub mod response_shape_compare;
#[cfg(test)]
pub mod response_shape_compare_test;
#[cfg(test)]
pub mod response_shape_test;
mod schema_constraint;
mod subgraph_constraint;

use std::fmt;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::validation::Valid;
use query_plan_analysis::AnalysisContext;

use crate::FederationError;
use crate::compat::coerce_executable_values;
use crate::correctness::response_shape_compare::ComparisonError;
use crate::correctness::response_shape_compare::PossibleTypes;
use crate::correctness::response_shape_compare::compare_response_shapes_with_constraint;
use crate::query_plan::QueryPlan;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Public API

#[derive(derive_more::From, Debug)]
pub enum CorrectnessError {
    /// Correctness checker's own error
    FederationError(FederationError),
    /// Error in the input that is subject to comparison
    ComparisonError(ComparisonError),
}

impl fmt::Display for CorrectnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorrectnessError::FederationError(err) => {
                write!(f, "Correctness check failed to complete: {err}")
            }
            CorrectnessError::ComparisonError(err) => {
                write!(f, "Correctness error found:\n{}", err.description())
            }
        }
    }
}

/// Check if `this` response shape is a subset of `other` under a single schema.
/// - Both response shapes must be derived from the given schema.
pub fn compare_response_shapes(
    schema: &ValidFederationSchema,
    this: &response_shape::ResponseShape,
    other: &response_shape::ResponseShape,
) -> Result<(), ComparisonError> {
    let path_constraint = schema_constraint::SchemaConstraint::new(schema);
    let possible_types = PossibleTypes::All; // unconstrained at the top level
    let assumption = response_shape::Clause::default(); // empty assumption at the top level
    compare_response_shapes_with_constraint(
        &path_constraint,
        &possible_types,
        &assumption,
        this,
        other,
    )
}

/// Check if `this` response shape is a subset of `other` in a federated context (supergraph
/// schema plus subgraph schemas).
/// - The response shapes may use any fields from the supergraph schema.
/// - The schema constraint (from the supergraph schema) is the base; the subgraph constraint
///   is an extra oracle on top.
pub fn compare_response_shapes_in_supergraph(
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    this: &response_shape::ResponseShape,
    other: &response_shape::ResponseShape,
) -> Result<(), ComparisonError> {
    let path_constraint = (
        schema_constraint::SchemaConstraint::new(supergraph_schema),
        subgraph_constraint::SubgraphConstraint::new(subgraphs_by_name),
    );
    let possible_types = PossibleTypes::All; // unconstrained at the top level
    let assumption = response_shape::Clause::default(); // empty assumption at the top level
    compare_response_shapes_with_constraint(
        &path_constraint,
        &possible_types,
        &assumption,
        this,
        other,
    )
}

/// Check if `this`'s response shape is a subset of `other`'s response shape.
pub fn compare_operations(
    schema: &ValidFederationSchema,
    this: &Valid<ExecutableDocument>,
    other: &Valid<ExecutableDocument>,
) -> Result<(), CorrectnessError> {
    let this_rs = response_shape::compute_response_shape_for_operation(this, schema)?;
    let other_rs = response_shape::compute_response_shape_for_operation(other, schema)?;
    tracing::debug!(
        "compare_operations:\nResponse shape (left): {this_rs}\nResponse shape (right): {other_rs}"
    );
    Ok(compare_response_shapes(schema, &this_rs, &other_rs)?)
}

/// The response-shape query plan checker, which predates the port of the Lean `checkQueryPlan`
/// model and is kept reachable so the two can be run against each other.
///
/// Its implementation still lives in this module rather than under `legacy`, to keep the change
/// that introduced the new checker small.
pub mod legacy {
    pub use super::check_plan_with_response_shapes as check_plan;
}

/// Checks the plan checker can make but does not make by default.
///
/// Each one reports a real query-planner defect that the planner cannot currently avoid, so
/// turning it on will fault plans the router ships today. They are options rather than findings
/// so that a corpus run can ask how widespread each defect is without the checker rejecting
/// working plans in ordinary use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckerOptions {
    /// Report a flatten path element narrowed to no runtime type at all — FED-516.
    ///
    /// The planner writes an empty type condition when it has decided the position admits
    /// nothing, and the judgement is right: the fetch under such a path can never run. It is dead
    /// code in the plan rather than a soundness violation, and the checker is otherwise silent
    /// about it, since a path that reaches no type mounts no requirement.
    pub check_empty_flatten_path_type_condition: bool,

    /// Report two `@requires` on one entity that demand the same response key with different
    /// arguments — FED-504.
    ///
    /// A fetch can only make one of the two calls, so one of the two fields is fed data it did
    /// not ask for. Neither the plan nor the rest of this checker can see it: the plan's own
    /// `requires` entry has had its arguments dropped by `trim_requires_selection_set`, and the
    /// demand the entry is compared against is a concatenation of the two field sets, which
    /// `query_compare` reads as one call because GraphQL validation would have merged them.
    pub check_requires_conflict: bool,
}

/// Check that a query plan is correct for a client operation.
///
/// This is [`query_plan_check`], the port of the Lean `checkQueryPlan` model. It decides the same
/// question as [`legacy::check_plan`] by a different route, and asks two things legacy does not:
/// that every entity case of a fetch is covered by some `requires` entry, and that every
/// contextual value a fetch reads is already fetched wherever the fetch runs.
pub fn check_plan(
    api_schema: &ValidFederationSchema,
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
) -> Result<(), CorrectnessError> {
    check_plan_with_options(
        api_schema,
        supergraph_schema,
        subgraphs_by_name,
        operation_doc,
        plan,
        CheckerOptions::default(),
    )
}

/// [`check_plan`], with the optional checks in [`CheckerOptions`] turned on or off.
pub fn check_plan_with_options(
    api_schema: &ValidFederationSchema,
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
    options: CheckerOptions,
) -> Result<(), CorrectnessError> {
    let operation_doc = coerce_input_operation(api_schema, operation_doc)?;
    query_plan_check::check_plan_with_options(
        supergraph_schema,
        subgraphs_by_name,
        &operation_doc,
        plan,
        options,
    )?;
    Ok(())
}

/// Coerce constant expressions in the input operation document, since the query planner does it
/// for subgraph fetch operations and the two are compared argument by argument.
// This may become unnecessary in the future; see ROUTER-816.
fn coerce_input_operation(
    api_schema: &ValidFederationSchema,
    operation_doc: &Valid<ExecutableDocument>,
) -> Result<Valid<ExecutableDocument>, FederationError> {
    let mut operation_doc = operation_doc.clone().into_inner();
    coerce_executable_values(api_schema.schema(), &mut operation_doc);
    Ok(operation_doc.validate(api_schema.schema())?)
}

/// Check the correctness of the query plan against the schema and input operation by comparing
/// the response shape of the input operation and the response shape of the query plan.
/// - The input operation's response shape is supposed to be a subset of the input operation's.
pub fn check_plan_with_response_shapes(
    api_schema: &ValidFederationSchema,
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
) -> Result<(), CorrectnessError> {
    let operation_doc = coerce_input_operation(api_schema, operation_doc)?;

    let op_rs = response_shape::compute_response_shape_for_operation(&operation_doc, api_schema)?;
    let root_type = response_shape::compute_the_root_type_condition_for_operation(&operation_doc)?;
    let context = AnalysisContext::new(supergraph_schema.clone(), subgraphs_by_name);
    let plan_rs =
        query_plan_analysis::interpret_query_plan(&context, &root_type, plan).map_err(|e| {
            ComparisonError::new(format!(
                "Failed to compute the response shape from query plan:\n{e}"
            ))
        })?;
    tracing::debug!(
        "check_plan:\nOperation response shape: {op_rs}\nQuery plan response shape: {plan_rs}"
    );

    // Note: The comparison must use the supergraph schema (not the API schema), since `plan_rs`
    //       may contain any fields from the supergraph schema. The `op_rs` side is indeed
    //       constrained to the API schema, which is a subset of the supergraph schema.
    compare_response_shapes_in_supergraph(supergraph_schema, subgraphs_by_name, &op_rs, &plan_rs)
        .map_err(|e| {
            ComparisonError::new(format!(
                "Response shape from query plan does not match response shape from input operation:\n{e}"
            ))
        })?;
    Ok(())
}
