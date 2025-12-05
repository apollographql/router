use ahash::HashMap;
use apollo_compiler::ExecutableDocument;

use crate::graphql;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::cost_calculator::static_cost::StaticCostCalculator;
use crate::plugins::demand_control::strategy::StrategyImpl;
use crate::services::execution;
use crate::services::subgraph;

/// This strategy will reject requests if the estimated cost of the request exceeds the maximum cost.
pub(crate) struct StaticEstimated {
    // The estimated value of the demand
    pub(crate) max: f64,
    pub(crate) subgraph_limits: HashMap<String, Option<f64>>,
    pub(crate) all_default_limit: Option<f64>,
    pub(crate) cost_calculator: StaticCostCalculator,
}

impl StrategyImpl for StaticEstimated {
    fn on_execution_request(&self, request: &execution::Request) -> Result<(), DemandControlError> {
        let variables = &request.supergraph_request.body().variables;

        // Calculate total cost
        let total_cost = self
            .cost_calculator
            .planned(&request.query_plan, variables)?;

        // Calculate per-subgraph costs
        let subgraph_costs = self
            .cost_calculator
            .planned_per_subgraph(&request.query_plan, variables)?;

        request
            .context
            .insert_cost_strategy("static_estimated".to_string())?;
        request.context.insert_cost_result("COST_OK".to_string())?;
        request.context.insert_estimated_cost(total_cost)?;

        // Store per-subgraph costs in context for telemetry
        request
            .context
            .insert_all_subgraph_costs(subgraph_costs.clone())?;

        // Check global limit first
        if total_cost > self.max {
            let error = DemandControlError::EstimatedCostTooExpensive {
                estimated_cost: total_cost,
                max_cost: self.max,
            };
            request
                .context
                .insert_cost_result(error.code().to_string())?;
            return Err(error);
        }

        // Check per-subgraph limits
        for (subgraph_name, cost) in subgraph_costs {
            // Check if there's a specific limit for this subgraph
            let max_cost = self
                .subgraph_limits
                .get(&subgraph_name)
                .copied()
                .flatten()
                // Fall back to "all" default if no specific limit
                .or(self.all_default_limit);

            if let Some(max_cost) = max_cost
                && cost > max_cost
            {
                tracing::warn!(
                    subgraph_name = %subgraph_name,
                    estimated_cost = cost,
                    max_cost = max_cost,
                    "Subgraph cost limit exceeded"
                );
                let error = DemandControlError::SubgraphCostTooExpensive {
                    subgraph_name: subgraph_name.clone(),
                    estimated_cost: cost,
                    max_cost,
                };
                request
                    .context
                    .insert_cost_result(error.code().to_string())?;
                return Err(error);
            }
        }

        Ok(())
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
            let cost = self.cost_calculator.actual(
                request,
                response,
                &context
                    .extensions()
                    .with_lock(|lock| lock.get().cloned())
                    .unwrap_or_default(),
            )?;
            context.insert_actual_cost(cost)?;
        }
        Ok(())
    }
}
