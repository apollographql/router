use std::sync::Arc;

use apollo_compiler::ExecutableDocument;

use crate::configuration::subgraph::SubgraphConfiguration;
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
    pub(crate) subgraph_maxes: Arc<SubgraphConfiguration<Option<f64>>>,
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
                let mut errors = vec![];

                let cost = cost_by_subgraph.total();

                if cost > self.max {
                    errors.push(DemandControlError::EstimatedCostTooExpensive {
                        estimated_cost: cost,
                        max_cost: self.max,
                    });
                }

                // see if any individual subgraph exceeded its limit
                for (subgraph, subgraph_cost) in cost_by_subgraph.iter() {
                    if let Some(max) = self.subgraph_max(subgraph)
                        && *subgraph_cost > max
                    {
                        errors.push(DemandControlError::EstimatedSubgraphCostTooExpensive {
                            subgraph: subgraph.clone(),
                            estimated_cost: *subgraph_cost,
                            max_cost: max,
                        });
                    }
                }

                request
                    .context
                    .insert_cost_strategy("static_estimated".to_string())?;
                request.context.insert_estimated_cost(cost)?;
                request
                    .context
                    .insert_estimated_cost_by_subgraph(cost_by_subgraph)?;

                if !errors.is_empty() {
                    let error = DemandControlError::MultipleCostsTooExpensive(errors);
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
            ref subgraphs,
        } = plugin.config.strategy
        else {
            panic!("must provide static_estimated config");
        };
        let strategy = plugin
            .strategy_factory
            .create_static_estimated_strategy(list_size, max, subgraphs);
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
        let config = include_str!("../fixtures/invalid_per_subgraph.yaml");
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
