use displaydoc::Display;
use serde::Serialize;
use thiserror::Error;

use crate::configuration::subgraph::SubgraphConfiguration;

#[apollo_configuration::configuration]
#[derive(Error, Display, Serialize)]
pub(crate) enum BatchingMode {
    /// batch_http_link
    #[config(default)]
    BatchHttpLink,
}

/// Common options for configuring subgraph batching
#[apollo_configuration::configuration]
#[derive(Serialize)]
pub(crate) struct CommonBatchingConfig {
    /// Whether this batching config should be enabled
    pub(crate) enabled: bool,
}

/// Configuration for Batching
#[apollo_configuration::configuration]
#[derive(Serialize)]
pub(crate) struct BatchingConfig {
    /// Activates Batching (disabled by default)
    pub(crate) enabled: bool,

    /// Batching mode
    pub(crate) mode: BatchingMode,

    /// Subgraph options for batching
    // Generic with a hand-written merging Deserialize, which the macro can't express.
    #[config(skip_validate)]
    pub(crate) subgraph: Option<SubgraphConfiguration<CommonBatchingConfig>>,

    /// Maximum size for a batch
    pub(crate) maximum_size: Option<usize>,
}

impl BatchingConfig {
    // Check if we should enable batching for a particular subgraph (service_name)
    pub(crate) fn batch_include(&self, service_name: &str) -> bool {
        match &self.subgraph {
            Some(subgraph_batching_config) => {
                // Override by checking if all is enabled
                if subgraph_batching_config.all.enabled {
                    // If it is, require:
                    // - no subgraph entry OR
                    // - an enabled subgraph entry
                    subgraph_batching_config
                        .subgraphs
                        .get(service_name)
                        .is_none_or(|x| x.enabled)
                } else {
                    // If it isn't, require:
                    // - an enabled subgraph entry
                    subgraph_batching_config
                        .subgraphs
                        .get(service_name)
                        .is_some_and(|x| x.enabled)
                }
            }
            None => false,
        }
    }

    pub(crate) fn exceeds_batch_size<T>(&self, batch: &[T]) -> bool {
        match self.maximum_size {
            Some(maximum_size) => batch.len() > maximum_size,
            None => false,
        }
    }
}
