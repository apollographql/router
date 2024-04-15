use apollo_compiler::ExecutableDocument;

use crate::plugins::demand_control::strategy::StrategyImpl;
use crate::plugins::demand_control::test::{TestError, TestStage};
use crate::plugins::demand_control::DemandControlError;
use crate::services::execution::Request;
use crate::services::subgraph::Response;

pub(crate) struct Test {
    pub(crate) stage: TestStage,
    pub(crate) error: TestError,
}

impl StrategyImpl for Test {
    fn on_execution_request(&self, _request: &Request) -> Result<(), DemandControlError> {
        match self {
            Test {
                stage: TestStage::OnExecutionRequest,
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
                stage: TestStage::OnSubgraphRequest,
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
                stage: TestStage::OnSubgraphResponse,
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
                stage: TestStage::OnExecutionResponse,
                error,
            } => Err(error.into()),
            _ => Ok(()),
        }
    }
}
