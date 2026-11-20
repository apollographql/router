use std::sync::Arc;

use tower::BoxError;

use self::join_batch_requests_layer::JoinBatchRequestsLayer;
use self::query_plan_analysis_layer::BatchQueryPlanAnalysisLayer;
use self::split_batch_request_layer::SplitBatchRequestLayer;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;

pub(crate) mod config;
pub(crate) use state::BatchQuery;
mod join_batch_requests_layer;
mod query_plan_analysis_layer;
mod split_batch_request_layer;
mod state;

pub(crate) struct BatchingPlugin {
    config: Arc<config::BatchingConfig>,
}

#[async_trait::async_trait]
impl Plugin for BatchingPlugin {
    type Config = config::BatchingConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            config: Arc::new(init.config),
        })
    }
}

impl BatchingPlugin {
    /// Intercept requests that are part of a batch, and batch them up together into a single request
    /// per subgraph.
    ///
    /// All subgraph requests for a batch, even for different subgraphs, are sent at the same time once
    /// the whole batch is ready.
    pub(crate) fn join_batch_requests_layer(
        &self,
        subgraph_name: impl Into<Arc<str>>,
    ) -> JoinBatchRequestsLayer {
        JoinBatchRequestsLayer::new(subgraph_name)
    }

    pub(crate) fn query_plan_analysis_layer(&self) -> BatchQueryPlanAnalysisLayer {
        BatchQueryPlanAnalysisLayer::new()
    }

    pub(crate) fn split_batch_request_layer(&self) -> SplitBatchRequestLayer {
        SplitBatchRequestLayer::new(self.config.clone())
    }
}

register_plugin!("apollo", "batching", BatchingPlugin);
