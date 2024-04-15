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
    fn on_execution_request(&self, _request: &Request) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::ExecutionRequest,
                error,
            } => Err(error.into()),
            _ => Ok(()),
        }
    }

    fn on_subgraph_request(
        &self,
        _request: &crate::services::subgraph::Request,
    ) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::SubgraphRequest,
                error,
            } => Err(error.into()),
            _ => Ok(()),
        }
    }

    fn on_subgraph_response(
        &self,
        _request: &ExecutableDocument,
        _response: &Response,
    ) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::SubgraphResponse,
                error,
            } => Err(error.into()),
            _ => Ok(()),
        }
    }

    fn on_execution_response(
        &self,
        _request: &ExecutableDocument,
        _response: &crate::graphql::Response,
    ) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::ExecutionResponse,
                error,
            } => Err(error.into()),
            _ => Ok(()),
        }
    }
}
