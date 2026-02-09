use std::sync::Arc;

use ahash::HashMap;
use apollo_compiler::ExecutableDocument;

use crate::Context;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::plugins::demand_control::ActualCostMode;
use crate::plugins::demand_control::DemandControlConfig;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::Mode;
use crate::plugins::demand_control::StrategyConfig;
use crate::plugins::demand_control::SubgraphStrategyConfig;
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
        request: &ExecutableDocument,
        response: &subgraph::Response,
        subgraph_name: &str,
    ) -> Result<(), DemandControlError> {
        match self
            .inner
            .on_subgraph_response(request, response, subgraph_name)
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

    pub(crate) fn create_static_estimated_strategy(
        &self,
        list_size: u32,
        max: f64,
        actual_cost_mode: ActualCostMode,
        subgraphs: &SubgraphConfiguration<SubgraphStrategyConfig>,
    ) -> StaticEstimated {
        let subgraph_maxes = subgraphs.extract(|config| config.max);
        let subgraph_list_sizes = subgraphs.extract(|config| config.list_size);
        StaticEstimated {
            max,
            subgraph_maxes,
            actual_cost_mode,
            cost_calculator: StaticCostCalculator::new(
                self.supergraph_schema.clone(),
                self.subgraph_schemas.clone(),
                Arc::new(subgraph_list_sizes),
                list_size,
            ),
        }
    }

    pub(crate) fn create(&self) -> Strategy {
        let strategy: Arc<dyn StrategyImpl> = match &self.config.strategy {
            StrategyConfig::StaticEstimated {
                list_size,
                max,
                actual_cost_mode,
                subgraph,
            } => Arc::new(self.create_static_estimated_strategy(
                *list_size,
                *max,
                *actual_cost_mode,
                subgraph,
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
        request: &ExecutableDocument,
        response: &subgraph::Response,
        subgraph_name: &str,
    ) -> Result<(), DemandControlError>;
    fn on_execution_response(
        &self,
        context: &Context,
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<(), DemandControlError>;
}
