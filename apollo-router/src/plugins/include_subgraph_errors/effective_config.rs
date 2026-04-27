use std::collections::HashMap;

use itertools::Itertools;

use super::config::Config;
use super::config::ErrorMode;
use super::config::SubgraphConfig;
use crate::error::ConfigurationError;

#[derive(Debug, Default, Clone)]
pub(crate) struct EffectiveConfig {
    /// Default effective configuration applied if a subgraph isn't specifically listed.
    pub(crate) default: SubgraphEffectiveConfig,
    /// Per-subgraph effective configurations.
    pub(crate) subgraphs: HashMap<String, SubgraphEffectiveConfig>,
}

/// Generates the effective configuration by merging global and per-subgraph settings.
impl TryFrom<Config> for EffectiveConfig {
    type Error = ConfigurationError;

    fn try_from(config: Config) -> Result<Self, Self::Error> {
        let mut effective_config = EffectiveConfig {
            default: Self::default_config(&config),
            ..Default::default()
        };

        // Determine global defaults
        let default_config = &effective_config.default;

        // Calculate effective config for each specific subgraph
        for (name, subgraph_config) in &config.subgraphs {
            // Compute effective configuration by merging global and subgraph settings.
            let (
                effective_include_errors,
                effective_redact,
                effective_redact_service_name,
                effective_allow,
                effective_deny,
            ) = match subgraph_config {
                SubgraphConfig::Allow {
                    allow_extensions_keys: sub_allow,
                    redact_message: sub_redact,
                    redact_service_name: sub_redact_service_name,
                    exclude_global_keys,
                } => {
                    let redact = sub_redact.unwrap_or(default_config.redact_message);
                    let redact_service_name =
                        sub_redact_service_name.unwrap_or(default_config.redact_service_name);
                    match &default_config.allow_extensions_keys {
                        Some(global_allow) => {
                            let mut allow_list = global_allow
                                .iter()
                                .filter(|k| !exclude_global_keys.contains(k))
                                .cloned()
                                .collect::<Vec<_>>();
                            // Add subgraph's allow keys
                            allow_list.extend(sub_allow.iter().cloned());
                            (true, redact, redact_service_name, Some(allow_list), None)
                        }
                        None => (
                            true,
                            redact,
                            redact_service_name,
                            Some(sub_allow.to_vec()),
                            None,
                        ),
                    }
                }
                SubgraphConfig::Deny {
                    deny_extensions_keys: sub_deny,
                    redact_message: sub_redact,
                    redact_service_name: sub_redact_service_name,
                    exclude_global_keys,
                } => {
                    let redact = sub_redact.unwrap_or(default_config.redact_message);
                    let redact_service_name =
                        sub_redact_service_name.unwrap_or(default_config.redact_service_name);
                    match &default_config.deny_extensions_keys {
                        Some(global_deny) => {
                            let mut deny_list = global_deny
                                .iter()
                                .filter(|k| !exclude_global_keys.contains(k))
                                .cloned()
                                .collect::<Vec<_>>();
                            deny_list.extend(sub_deny.clone());
                            (true, redact, redact_service_name, None, Some(deny_list))
                        }
                        None => (
                            true,
                            redact,
                            redact_service_name,
                            None,
                            Some(sub_deny.to_vec()),
                        ),
                    }
                }
                SubgraphConfig::Included(enabled) => {
                    // Discard global allow/deny when subgraph is bool.
                    // Inherit redact_service_name from global when enabled, otherwise irrelevant.
                    let redact_service_name = if *enabled {
                        default_config.redact_service_name
                    } else {
                        false
                    };
                    (*enabled, false, redact_service_name, None, None)
                }
                SubgraphConfig::CommonOnly {
                    redact_message: sub_redact,
                    redact_service_name: sub_redact_service_name,
                    exclude_global_keys,
                } => {
                    let redact = sub_redact.unwrap_or(default_config.redact_message);
                    let redact_service_name =
                        sub_redact_service_name.unwrap_or(default_config.redact_service_name);
                    // Inherit global allow/deny lists when using CommonOnly
                    match config.all.clone() {
                        ErrorMode::Allow {
                            allow_extensions_keys,
                            ..
                        } => (
                            true,
                            redact,
                            redact_service_name,
                            Some(
                                allow_extensions_keys
                                    .iter()
                                    .filter(|k| !exclude_global_keys.contains(k))
                                    .cloned()
                                    .collect(),
                            ),
                            None,
                        ),
                        ErrorMode::Deny {
                            deny_extensions_keys,
                            ..
                        } => (
                            true,
                            redact,
                            redact_service_name,
                            None,
                            Some(
                                deny_extensions_keys
                                    .iter()
                                    .filter(|k| !exclude_global_keys.contains(k))
                                    .cloned()
                                    .collect(),
                            ),
                        ),
                        _ => (true, redact, redact_service_name, None, None),
                    }
                }
            };

            effective_config.subgraphs.insert(
                name.clone(),
                SubgraphEffectiveConfig {
                    include_errors: effective_include_errors,
                    redact_message: effective_redact,
                    redact_service_name: effective_redact_service_name,
                    allow_extensions_keys: effective_allow,
                    deny_extensions_keys: effective_deny,
                },
            );
        }

        Ok(effective_config)
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SubgraphEffectiveConfig {
    /// Whether errors from this subgraph should be included at all.
    pub(crate) include_errors: bool,
    /// Whether the error message should be redacted.
    pub(crate) redact_message: bool,
    /// Whether the service name should be removed from error messages and extensions.
    pub(crate) redact_service_name: bool,
    /// Set of extension keys explicitly allowed. If `None`, all are allowed unless denied.
    pub(crate) allow_extensions_keys: Option<Vec<String>>,
    /// Set of extension keys explicitly denied. Applied *after* allow list filtering.
    pub(crate) deny_extensions_keys: Option<Vec<String>>,
}

impl EffectiveConfig {
    fn default_config(config: &Config) -> SubgraphEffectiveConfig {
        let (
            global_include_errors,
            global_redact_message,
            global_redact_service_name,
            global_allow_keys,
            global_deny_keys,
        ) = match &config.all {
            ErrorMode::Included(enabled) => (*enabled, !*enabled, false, None, None), // Redact if not enabled
            ErrorMode::Allow {
                allow_extensions_keys,
                redact_message,
                redact_service_name,
            } => (
                true,
                *redact_message,
                *redact_service_name,
                Some(
                    allow_extensions_keys
                        .iter()
                        .sorted()
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                None,
            ),
            ErrorMode::Deny {
                deny_extensions_keys,
                redact_message,
                redact_service_name,
            } => (
                true,
                *redact_message,
                *redact_service_name,
                None,
                Some(
                    deny_extensions_keys
                        .iter()
                        .sorted()
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
            ),
        };
        SubgraphEffectiveConfig {
            include_errors: global_include_errors,
            redact_message: global_redact_message,
            redact_service_name: global_redact_service_name,
            allow_extensions_keys: global_allow_keys,
            deny_extensions_keys: global_deny_keys,
        }
    }
}
