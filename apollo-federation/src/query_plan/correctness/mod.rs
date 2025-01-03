pub mod response_shape;
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
    _plan: &QueryPlan,
) -> Result<(), FederationError> {
    let rs = response_shape::compute_response_shape(operation_doc, schema)?;
    println!("\nResponse shape from operation:\n{rs}");
    Ok(())
}
