use derive_more::Display;
use serde::Serialize;

use crate::configuration::subgraph::SubgraphConfiguration;

#[apollo_configuration::configuration]
#[derive(Display, Serialize)]
pub(crate) enum BatchingMode {
    /// The Batch HTTP Link mode sends multiple GraphQL operations in a JSON array.
    #[config(default)]
    #[display("batch_http_link")]
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

#[cfg(test)]
mod tests {
    use apollo_configuration::ConfigParser;

    use super::*;

    fn parse(yaml: &str) -> BatchingConfig {
        ConfigParser::<BatchingConfig>::builder()
            .build()
            .unwrap()
            .parse_yaml(yaml)
            .unwrap()
    }

    #[test]
    fn subgraph_all_enabled() {
        let config = parse(
            r#"
            enabled: true
            mode: batch_http_link
            subgraph:
              all:
                enabled: true
            "#,
        );

        assert!(config.batch_include("anything"));
    }

    #[test]
    fn subgraph_all_disabled() {
        let config = parse(
            r#"
            enabled: true
            mode: batch_http_link
            subgraph:
              all:
                enabled: false
            "#,
        );

        assert!(!config.batch_include("anything"));
    }

    #[test]
    fn subgraph_accounts_enabled() {
        let config = parse(
            r#"
            enabled: true
            mode: batch_http_link
            subgraph:
              all:
                enabled: false
              subgraphs:
                accounts:
                  enabled: true
            "#,
        );

        assert!(!config.batch_include("anything"));
        assert!(config.batch_include("accounts"));
    }

    #[test]
    fn subgraph_accounts_disabled() {
        let config = parse(
            r#"
            enabled: true
            mode: batch_http_link
            subgraph:
              all:
                enabled: false
              subgraphs:
                accounts:
                  enabled: false
            "#,
        );

        assert!(!config.batch_include("anything"));
        assert!(!config.batch_include("accounts"));
    }

    #[test]
    fn subgraph_accounts_override_disabled() {
        let config = parse(
            r#"
            enabled: true
            mode: batch_http_link
            subgraph:
              all:
                enabled: true
              subgraphs:
                accounts:
                  enabled: false
            "#,
        );

        assert!(config.batch_include("anything"));
        assert!(!config.batch_include("accounts"));
    }

    #[test]
    fn mode_defaults_to_batch_http_link() {
        let config = parse("enabled: true");

        assert!(matches!(config.mode, BatchingMode::BatchHttpLink));
    }

    #[test]
    fn maximum_size_is_unset_by_default() {
        let config = parse(
            r#"
            enabled: true
            mode: batch_http_link
            "#,
        );

        assert_eq!(config.maximum_size, None);
    }

    #[test]
    fn maximum_size_is_specified() {
        let config = parse(
            r#"
            enabled: true
            mode: batch_http_link
            maximum_size: 10
            "#,
        );

        assert_eq!(config.maximum_size, Some(10));
    }
}
