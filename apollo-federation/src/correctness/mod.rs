pub mod query_plan_analysis;
#[cfg(test)]
pub mod query_plan_analysis_test;
mod query_plan_soundness;
#[cfg(test)]
pub mod query_plan_soundness_test;
pub mod response_shape;
pub mod response_shape_compare;
#[cfg(test)]
pub mod response_shape_compare_test;
#[cfg(test)]
pub mod response_shape_test;
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
    Ok(response_shape_compare::compare_response_shapes(
        &this_rs, &other_rs,
    )?)
}

/// Check the correctness of the query plan against the schema and input operation by comparing
/// the response shape of the input operation and the response shape of the query plan.
/// - The input operation's response shape is supposed to be a subset of the input operation's.
pub fn check_plan(
    api_schema: &ValidFederationSchema,
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
) -> Result<(), CorrectnessError> {
    // Coerce constant expressions in the input operation document since query planner does it for
    // subgraph fetch operations. But, this may be unnecessary in the future (see ROUTER-816).
    let mut operation_doc = operation_doc.clone().into_inner();
    coerce_executable_values(api_schema.schema(), &mut operation_doc);
    let operation_doc = operation_doc
        .validate(api_schema.schema())
        .map_err(FederationError::from)?;

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

    let path_constraint = subgraph_constraint::SubgraphConstraint::at_root(subgraphs_by_name);
    let assumption = response_shape::Clause::default(); // empty assumption at the top level
    compare_response_shapes_with_constraint(&path_constraint, &assumption, &op_rs, &plan_rs).map_err(|e| {
        ComparisonError::new(format!(
            "Response shape from query plan does not match response shape from input operation:\n{e}"
        ))
    })?;
    Ok(())
}
