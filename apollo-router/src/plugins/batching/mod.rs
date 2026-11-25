use std::sync::Arc;

use tower::BoxError;

use self::join_batch_requests_layer::JoinBatchRequestsLayer;
use self::query_plan_analysis_layer::BatchQueryPlanAnalysisLayer;
use self::split_batch_request_layer::SplitBatchRequestLayer;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::uplink::license_enforcement::AllowedFeature;
use crate::uplink::license_enforcement::LicensedSetting;

pub(crate) mod config;
pub(crate) use state::BatchQuery;
mod join_batch_requests_layer;
mod metrics;
mod query_plan_analysis_layer;
mod split_batch_request_layer;
mod state;

pub(crate) struct BatchingPlugin {
    config: Arc<config::BatchingConfig>,
}

#[async_trait::async_trait]
impl PluginPrivate for BatchingPlugin {
    type Config = config::BatchingConfig;

    fn licensed_features(
        config: &Self::Config,
        _supergraph: &apollo_compiler::Schema,
    ) -> Vec<LicensedSetting> {
        if config.enabled {
            vec![LicensedSetting {
                feature: AllowedFeature::Batching,
                path: "batching.enabled",
            }]
        } else {
            Vec::new()
        }
    }

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
    ///
    /// If batching is not enabled, it passes requests straight through.
    pub(crate) fn join_batch_requests_layer(
        &self,
        subgraph_name: impl Into<Arc<str>>,
    ) -> tower::util::Either<JoinBatchRequestsLayer, tower::layer::util::Identity> {
        let subgraph_name = subgraph_name.into();
        tower::util::option_layer(
            (self.config.enabled && self.config.batch_include(&subgraph_name))
                .then(|| JoinBatchRequestsLayer::new(subgraph_name)),
        )
    }

    /// Analyze the query plan for a batch query.
    /// Reject the request if it contains any subscription or defer nodes, which is not
    /// supported in batch requests.
    ///
    /// If batching is not enabled, it passes requests straight through.
    pub(crate) fn query_plan_analysis_layer(
        &self,
    ) -> tower::util::Either<BatchQueryPlanAnalysisLayer, tower::layer::util::Identity> {
        tower::util::option_layer(self.config.enabled.then(BatchQueryPlanAnalysisLayer::new))
    }

    /// If batching is enabled, split batch requests into multiple requests that flow
    /// separately through the rest of the pipeline, and stitch the responses back
    /// together as a JSON array.
    ///
    /// If batching is not enabled, it passes requests straight through.
    pub(crate) fn split_batch_request_layer(
        &self,
    ) -> tower::util::Either<SplitBatchRequestLayer, tower::layer::util::Identity> {
        tower::util::option_layer(
            self.config
                .enabled
                .then(|| SplitBatchRequestLayer::new(self.config.clone())),
        )
    }
}

register_private_plugin!("apollo", "batching", BatchingPlugin);
