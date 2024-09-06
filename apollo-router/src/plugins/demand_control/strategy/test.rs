use apollo_compiler::ExecutableDocument;

use crate::plugins::demand_control::strategy::StrategyImpl;
use crate::plugins::demand_control::test::TestError;
use crate::plugins::demand_control::test::TestStage;
use crate::plugins::demand_control::DemandControlError;
use crate::services::execution::Request;
use crate::services::subgraph::Response;

/// Test strategy for demand control.
/// Can be configured to fail at different stages of the request processing.
pub(crate) struct Test {
    pub(crate) stage: TestStage,
    pub(crate) error: TestError,
}

impl StrategyImpl for Test {
    fn on_execution_request(&self, request: &Request) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::ExecutionRequest,
                error,
            } => {
                let error: DemandControlError = error.into();
                request
                    .context
                    .insert_cost_result(error.code().to_string())?;
                Err(error)
            }
            _ => Ok(()),
        }
    }

    fn on_subgraph_request(
        &self,
        request: &crate::services::subgraph::Request,
    ) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::SubgraphRequest,
                error,
            } => {
                let error: DemandControlError = error.into();
                request
                    .context
                    .insert_cost_result(error.code().to_string())?;
                Err(error)
            }
            _ => Ok(()),
        }
    }

    fn on_subgraph_response(
        &self,
        _request: &ExecutableDocument,
        response: &Response,
    ) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::SubgraphResponse,
                error,
            } => {
                let error: DemandControlError = error.into();
                response
                    .context
                    .insert_cost_result(error.code().to_string())?;
                Err(error)
            }
            _ => Ok(()),
        }
    }

    fn on_execution_response(
        &self,
        context: &crate::Context,
        _request: &ExecutableDocument,
        _response: &crate::graphql::Response,
    ) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::ExecutionResponse,
                error,
            } => {
                let error: DemandControlError = error.into();
                context.insert_cost_result(error.code().to_string())?;
                Err(error)
            }
            _ => Ok(()),
        }
    }
}
