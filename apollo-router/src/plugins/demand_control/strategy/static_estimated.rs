use apollo_compiler::ExecutableDocument;

use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::plugins::demand_control::ActualCostMode;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::cost_calculator::static_cost::StaticCostCalculator;
use crate::plugins::demand_control::strategy::StrategyImpl;
use crate::services::execution;
use crate::services::subgraph;

/// This strategy will reject requests if the estimated cost of the request exceeds the maximum cost.
pub(crate) struct StaticEstimated {
    // The estimated value of the demand
    pub(crate) max: f64,
    pub(crate) subgraph_maxes: SubgraphConfiguration<Option<f64>>,
    pub(crate) actual_cost_mode: ActualCostMode,
    pub(crate) cost_calculator: StaticCostCalculator,
}

impl StaticEstimated {
    fn subgraph_max(&self, subgraph_name: &str) -> Option<f64> {
        *self.subgraph_maxes.get(subgraph_name)
    }
}

impl StrategyImpl for StaticEstimated {
    fn on_execution_request(&self, request: &execution::Request) -> Result<(), DemandControlError> {
        self.cost_calculator
            .planned(
                &request.query_plan,
                &request.supergraph_request.body().variables,
            )
            .and_then(|cost_by_subgraph| {
                let cost = cost_by_subgraph.total();
                request
                    .context
                    .insert_cost_strategy("static_estimated".to_string())?;
                request.context.insert_estimated_cost(cost)?;
                request
                    .context
                    .insert_estimated_cost_by_subgraph(cost_by_subgraph)?;

                if cost > self.max {
                    let error = DemandControlError::EstimatedCostTooExpensive {
                        estimated_cost: cost,
                        max_cost: self.max,
                    };
                    request
                        .context
                        .insert_cost_result(error.code().to_string())?;
                    Err(error)
                } else {
                    request.context.insert_cost_result("COST_OK".to_string())?;
                    Ok(())
                }
            })
    }

    fn on_subgraph_request(&self, _request: &subgraph::Request) -> Result<(), DemandControlError> {
        // TODO: reject subgraph requests when the total subgraph cost exceeds the subgraph max.
        Ok(())
    }

    fn on_subgraph_response(
        &self,
        request: &ExecutableDocument,
        response: &subgraph::Response,
        subgraph_name: &str,
    ) -> Result<(), DemandControlError> {
        if !matches!(self.actual_cost_mode, ActualCostMode::BySubgraph) {
            return Ok(());
        }

        let subgraph_response_body = response.response.body();
        let cost = self.cost_calculator.actual(
            request,
            subgraph_response_body,
            &response
                .context
                .extensions()
                .with_lock(|lock| lock.get().cloned())
                .unwrap_or_default(),
        )?;

        response
            .context
            .update_actual_cost_by_subgraph(subgraph_name, cost)?;

        Ok(())
    }

    fn on_execution_response(
        &self,
        context: &crate::Context,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<(), DemandControlError> {
        if response.data.is_none() {
            return Ok(());
        }

        let cost = match self.actual_cost_mode {
            ActualCostMode::BySubgraph => context
                .get_actual_cost_by_subgraph()?
                .map_or(0.0, |cost| cost.total()),
            ActualCostMode::Legacy => self.cost_calculator.actual(
                request,
                response,
                &context
                    .extensions()
                    .with_lock(|lock| lock.get().cloned())
                    .unwrap_or_default(),
            )?,
        };

        context.insert_actual_cost(cost)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tower::BoxError;

    use super::StaticEstimated;
    use crate::plugins::demand_control::DemandControl;
    use crate::plugins::demand_control::StrategyConfig;
    use crate::plugins::test::PluginTestHarness;

    async fn load_config_and_extract_strategy(
        config: &'static str,
    ) -> Result<StaticEstimated, BoxError> {
        let schema_str =
            include_str!("../cost_calculator/fixtures/basic_supergraph_schema.graphql");
        let plugin = PluginTestHarness::<DemandControl>::builder()
            .config(config)
            .schema(schema_str)
            .build()
            .await?;

        let StrategyConfig::StaticEstimated {
            list_size,
            max,
            actual_cost_mode,
            ref subgraph,
        } = plugin.config.strategy
        else {
            panic!("must provide static_estimated config");
        };

        let strategy = plugin.strategy_factory.create_static_estimated_strategy(
            list_size,
            max,
            actual_cost_mode,
            subgraph,
        );
        Ok(strategy)
    }

    #[tokio::test]
    async fn test_per_subgraph_configuration_inheritance() {
        let config = include_str!("../fixtures/per_subgraph_inheritance.yaml");

        let strategy = load_config_and_extract_strategy(config).await.unwrap();
        assert_eq!(strategy.subgraph_max("reviews").unwrap(), 2.0);
        assert_eq!(strategy.subgraph_max("products").unwrap(), 5.0);
        assert_eq!(strategy.subgraph_max("users").unwrap(), 5.0);
    }

    #[tokio::test]
    async fn test_per_subgraph_configuration_no_inheritance() {
        let config = include_str!("../fixtures/per_subgraph_no_inheritance.yaml");

        let strategy = load_config_and_extract_strategy(config).await.unwrap();
        assert_eq!(strategy.subgraph_max("reviews").unwrap(), 2.0);
        assert!(strategy.subgraph_max("products").is_none());
        assert!(strategy.subgraph_max("users").is_none());
    }

    #[tokio::test]
    async fn test_invalid_per_subgraph_configuration() {
        let config = include_str!("../fixtures/per_subgraph_invalid.yaml");
        let strategy_result = load_config_and_extract_strategy(config).await;

        match strategy_result {
            Ok(strategy) => {
                eprintln!("{:?}", strategy.subgraph_maxes);
                panic!("Expected error")
            }
            Err(err) => assert_eq!(
                &err.to_string(),
                "Maximum per-subgraph query cost for `products` is negative"
            ),
        };
    }
}
