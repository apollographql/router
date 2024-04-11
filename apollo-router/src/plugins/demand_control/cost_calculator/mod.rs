mod directives;
mod schema_aware_response;
pub(crate) mod static_cost;

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;

use crate::graphql;
use crate::plugins::demand_control::DemandControlError;
use crate::query_planner::QueryPlan;

pub(crate) trait CostCalculator: Send + Sync {
    fn estimate_query(
        &self,
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError>;

    fn estimate_plan(&self, query_plan: &QueryPlan) -> Result<f64, DemandControlError>;

    fn actual(
        &self,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<f64, DemandControlError>;
}
