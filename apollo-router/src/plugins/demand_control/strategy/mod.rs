use std::sync::Arc;

use ahash::HashMap;
use apollo_compiler::ExecutableDocument;

use crate::Context;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::plugins::demand_control::ActualCostComputationMode;
use crate::plugins::demand_control::DemandControlConfig;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::Mode;
use crate::plugins::demand_control::StrategyConfig;
use crate::plugins::demand_control::SubgraphStrategyLimit;
use crate::plugins::demand_control::cost_calculator::schema::DemandControlledSchema;
use crate::plugins::demand_control::cost_calculator::static_cost::StaticCostCalculator;
use crate::plugins::demand_control::strategy::static_estimated::StaticEstimated;
use crate::services::execution;
use crate::services::subgraph;

mod static_estimated;
#[cfg(test)]
mod test;

/// Strategy for a demand control exists for an entire request. It is Send and Sync but may contain state
/// such as the amount of budget remaining.
/// It is also responsible for updating metrics on what was rejected.
#[derive(Clone)]
pub(crate) struct Strategy {
    inner: Arc<dyn StrategyImpl>,
    pub(crate) mode: Mode,
}

impl Strategy {
    pub(crate) fn on_execution_request(
        &self,
        request: &execution::Request,
    ) -> Result<(), DemandControlError> {
        match self.inner.on_execution_request(request) {
            Err(e) if self.mode == Mode::Enforce => Err(e),
            _ => Ok(()),
        }
    }
    pub(crate) fn on_subgraph_request(
        &self,
        request: &subgraph::Request,
    ) -> Result<(), DemandControlError> {
        match self.inner.on_subgraph_request(request) {
            Err(e) if self.mode == Mode::Enforce => Err(e),
            _ => Ok(()),
        }
    }

    pub(crate) fn on_subgraph_response(
        &self,
        subgraph_name: String,
        request: &ExecutableDocument,
        response: &subgraph::Response,
    ) -> Result<(), DemandControlError> {
        match self
            .inner
            .on_subgraph_response(subgraph_name, request, response)
        {
            Err(e) if self.mode == Mode::Enforce => Err(e),
            _ => Ok(()),
        }
    }
    pub(crate) fn on_execution_response(
        &self,
        context: &Context,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<(), DemandControlError> {
        match self.inner.on_execution_response(context, request, response) {
            Err(e) if self.mode == Mode::Enforce => Err(e),
            _ => Ok(()),
        }
    }
}

pub(crate) struct StrategyFactory {
    config: DemandControlConfig,
    #[allow(dead_code)]
    supergraph_schema: Arc<DemandControlledSchema>,
    subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
}

impl StrategyFactory {
    pub(crate) fn new(
        config: DemandControlConfig,
        supergraph_schema: Arc<DemandControlledSchema>,
        subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
    ) -> Self {
        Self {
            config,
            supergraph_schema,
            subgraph_schemas,
        }
    }

    // Function extracted for use in tests - allows us to build a `StaticEstimated` directly rather
    // than a `impl StrategyImpl`
    fn create_static_estimated_strategy(
        &self,
        list_size: u32,
        max: f64,
        actual_cost_computation_mode: ActualCostComputationMode,
        subgraphs: &SubgraphConfiguration<SubgraphStrategyLimit>,
    ) -> StaticEstimated {
        let subgraph_list_sizes = Arc::new(subgraphs.extract(|strategy| strategy.list_size));
        let subgraph_maxes = Arc::new(subgraphs.extract(|strategy| strategy.max));
        let cost_calculator = StaticCostCalculator::new(
            self.supergraph_schema.clone(),
            self.subgraph_schemas.clone(),
            list_size,
            subgraph_list_sizes,
        );
        StaticEstimated {
            max,
            subgraph_maxes,
            actual_cost_computation_mode,
            cost_calculator,
        }
    }

    pub(crate) fn create(&self) -> Strategy {
        let strategy: Arc<dyn StrategyImpl> = match &self.config.strategy {
            StrategyConfig::StaticEstimated {
                list_size,
                max,
                actual_cost_computation_mode,
                subgraphs,
            } => Arc::new(self.create_static_estimated_strategy(
                *list_size,
                *max,
                *actual_cost_computation_mode,
                subgraphs,
            )),
            #[cfg(test)]
            StrategyConfig::Test { stage, error } => Arc::new(test::Test {
                stage: stage.clone(),
                error: error.clone(),
            }),
        };
        Strategy {
            mode: self.config.mode,
            inner: strategy,
        }
    }
}

pub(crate) trait StrategyImpl: Send + Sync {
    fn on_execution_request(&self, request: &execution::Request) -> Result<(), DemandControlError>;
    fn on_subgraph_request(&self, request: &subgraph::Request) -> Result<(), DemandControlError>;

    fn on_subgraph_response(
        &self,
        subgraph_name: String,
        request: &ExecutableDocument,
        response: &subgraph::Response,
    ) -> Result<(), DemandControlError>;
    fn on_execution_response(
        &self,
        context: &Context,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<(), DemandControlError>;
}
