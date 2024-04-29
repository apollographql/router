use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;

use crate::graphql;
use crate::plugins::demand_control::cost_calculator::static_cost::StaticCostCalculator;
use crate::plugins::demand_control::strategy::static_estimated::StaticEstimated;
use crate::plugins::demand_control::DemandControlConfig;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::Mode;
use crate::plugins::demand_control::StrategyConfig;
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
    mode: Mode,
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
        request: &ExecutableDocument,
        response: &subgraph::Response,
    ) -> Result<(), DemandControlError> {
        match self.inner.on_subgraph_response(request, response) {
            Err(e) if self.mode == Mode::Enforce => Err(e),
            _ => Ok(()),
        }
    }
    pub(crate) fn on_execution_response(
        &self,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<(), DemandControlError> {
        match self.inner.on_execution_response(request, response) {
            Err(e) if self.mode == Mode::Enforce => Err(e),
            _ => Ok(()),
        }
    }
}

pub(crate) struct StrategyFactory {
    config: DemandControlConfig,
    #[allow(dead_code)]
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<Schema>>>>,
}

impl StrategyFactory {
    pub(crate) fn new(
        config: DemandControlConfig,
        supergraph_schema: Arc<Valid<Schema>>,
        subgraph_schemas: Arc<HashMap<String, Arc<Valid<Schema>>>>,
    ) -> Self {
        Self {
            config,
            supergraph_schema,
            subgraph_schemas,
        }
    }

    pub(crate) fn create(&self) -> Strategy {
        let strategy: Arc<dyn StrategyImpl> = match &self.config.strategy {
            StrategyConfig::StaticEstimated { list_size, max } => Arc::new(StaticEstimated {
                max: *max,
                cost_calculator: StaticCostCalculator::new(
                    self.subgraph_schemas.clone(),
                    *list_size,
                ),
            }),
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
        request: &ExecutableDocument,
        response: &subgraph::Response,
    ) -> Result<(), DemandControlError>;
    fn on_execution_response(
        &self,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<(), DemandControlError>;
}
