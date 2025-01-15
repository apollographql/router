pub mod query_plan_analysis;
#[cfg(test)]
pub mod query_plan_analysis_test;
pub mod response_shape;
pub mod response_shape_compare;
#[cfg(test)]
pub mod response_shape_test;
mod subgraph_constraint;

use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;

use crate::query_plan::QueryPlan;
use crate::schema::ValidFederationSchema;
use crate::FederationError;

//==================================================================================================
// CheckFailure

pub struct CheckFailure {
    description: String,
}

impl CheckFailure {
    pub fn description(&self) -> &str {
        &self.description
    }

    fn new(description: String) -> CheckFailure {
        CheckFailure { description }
    }

    fn add_description(self: CheckFailure, description: &str) -> CheckFailure {
        CheckFailure {
            description: format!("{}\n{}", self.description, description),
        }
    }
}

//==================================================================================================
// check_plan

pub fn check_plan(
    schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
) -> Result<Option<CheckFailure>, FederationError> {
    let op_rs = response_shape::compute_response_shape_for_operation(operation_doc, schema)?;

    let root_type = response_shape::compute_the_root_type_condition_for_operation(operation_doc)?;
    let plan_rs = match query_plan_analysis::interpret_query_plan(schema, &root_type, plan) {
        Ok(rs) => rs,
        Err(e) => {
            return Ok(Some(CheckFailure::new(format!(
                "Failed to compute the response shape from query plan:\n{e}"
            ))));
        }
    };

    let root_constraint = subgraph_constraint::SubgraphConstraint::at_root(subgraphs_by_name);
    match response_shape_compare::compare_response_shapes(&root_constraint, &op_rs, &plan_rs) {
        Ok(_) => Ok(None),
        Err(e) => Ok(Some(e)),
    }
}
