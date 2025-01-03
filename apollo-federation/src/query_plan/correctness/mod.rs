pub mod query_plan_analysis;
#[cfg(test)]
pub mod query_plan_analysis_test;
pub mod response_shape;
pub mod response_shape_compare;
#[cfg(test)]
pub mod response_shape_test;

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;

use crate::query_plan::QueryPlan;
use crate::schema::ValidFederationSchema;
use crate::FederationError;

//==================================================================================================
// check_plan

pub fn check_plan(
    schema: &ValidFederationSchema,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
) -> Result<Option<response_shape_compare::MatchFailure>, FederationError> {
    let op_rs = response_shape::compute_response_shape_for_operation(operation_doc, schema)?;

    let root_type = response_shape::compute_the_root_type_condition_for_operation(operation_doc)?;
    let plan_rs = query_plan_analysis::interpret_query_plan(schema, &root_type, plan)?;

    match response_shape_compare::compare_response_shapes(&op_rs, &plan_rs) {
        Ok(_) => Ok(None),
        Err(e) => Ok(Some(e)),
    }
}
