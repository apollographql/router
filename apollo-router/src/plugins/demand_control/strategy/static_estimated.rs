use apollo_compiler::ExecutableDocument;

use crate::graphql;
use crate::plugins::demand_control::cost_calculator::static_cost::StaticCostCalculator;
use crate::plugins::demand_control::strategy::StrategyImpl;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::COST_ACTUAL_CONTEXT_KEY;
use crate::plugins::demand_control::COST_ESTIMATED_CONTEXT_KEY;
use crate::plugins::demand_control::COST_RESULT_CONTEXT_KEY;
use crate::plugins::demand_control::COST_STRATEGY_CONTEXT_KEY;
use crate::services::execution;
use crate::services::subgraph;

/// This strategy will reject requests if the estimated cost of the request exceeds the maximum cost.
pub(crate) struct StaticEstimated {
    // The estimated value of the demand
    pub(crate) max: f64,
    pub(crate) cost_calculator: StaticCostCalculator,
}

impl StrategyImpl for StaticEstimated {
    fn on_execution_request(&self, request: &execution::Request) -> Result<(), DemandControlError> {
        self.cost_calculator
            .planned(&request.query_plan)
            .and_then(|cost| {
                request
                    .context
                    .insert(COST_STRATEGY_CONTEXT_KEY, "static_estimated".to_string());
                request.context.insert(COST_ESTIMATED_CONTEXT_KEY, cost);

                if cost > self.max {
                    let error = DemandControlError::EstimatedCostTooExpensive {
                        estimated_cost: cost,
                        max_cost: self.max,
                    };
                    request
                        .context
                        .insert(COST_RESULT_CONTEXT_KEY, error.code().to_string());
                    Err(error)
                } else {
                    Ok(())
                }
            })
    }

    fn on_subgraph_request(&self, _request: &subgraph::Request) -> Result<(), DemandControlError> {
        Ok(())
    }

    fn on_subgraph_response(
        &self,
        _request: &ExecutableDocument,
        _response: &subgraph::Response,
    ) -> Result<(), DemandControlError> {
        Ok(())
    }

    fn on_execution_response(
        &self,
        context: &crate::Context,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<(), DemandControlError> {
        if response.data.is_some() {
            let cost = self.cost_calculator.actual(request, response)?;
            context.insert(COST_ACTUAL_CONTEXT_KEY, cost);
        }
        Ok(())
    }
}
