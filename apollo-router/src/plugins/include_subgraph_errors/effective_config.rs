use super::config::{Config, ErrorMode, SubgraphConfig, SubgraphConfigCommon};
use crate::error::ConfigurationError;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, Clone)]
pub(crate) struct EffectiveConfig {
    /// Default effective configuration applied if a subgraph isn't specifically listed.
    pub(crate) default: SubgraphEffectiveConfig,
    /// Per-subgraph effective configurations.
    pub(crate) subgraphs: HashMap<String, SubgraphEffectiveConfig>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SubgraphEffectiveConfig {
    /// Whether errors from this subgraph should be included at all.
    pub(crate) include_errors: bool,
    /// Whether the error message should be redacted.
    pub(crate) redact_message: bool,
    /// Set of extension keys explicitly allowed. If `None`, all are allowed unless denied.
    pub(crate) allow_extensions_keys: Option<HashSet<String>>,
    /// Set of extension keys explicitly denied. Applied *after* allow list filtering.
    pub(crate) deny_extensions_keys: Option<HashSet<String>>,
}

/// Generates the effective configuration by merging global and per-subgraph settings.
impl TryFrom<Config> for EffectiveConfig {
    type Error = ConfigurationError;

    fn try_from(config: Config) -> Result<Self, Self::Error> {
        let mut effective_config = EffectiveConfig::default();

        // Determine global defaults
        effective_config.default = Self::default_config(&config);
        let default_config = &effective_config.default;

        // Calculate effective config for each specific subgraph
        for (name, subgraph_config) in &config.subgraphs {
            // Compute effective configuration by merging global and subgraph settings.
            let (effective_allow, effective_deny, effective_redact) = match subgraph_config {
                SubgraphConfig::Allow {
                    allow_extensions_keys: sub_allow,
                    common:
                        SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys,
                        },
                } => {
                    let redact = sub_redact.unwrap_or(default_config.redact_message);
                    match &default_config.allow_extensions_keys {
                        Some(global_allow) => {
                            let mut allow_list = global_allow.clone();

                            // Remove any keys that should be overridden
                            if let Some(exclude_keys) = exclude_global_keys {
                                allow_list.retain(|key| !exclude_keys.contains(key));
                            }

                            // Add subgraph's allow keys
                            allow_list.extend(sub_allow.iter().cloned());
                            (Some(allow_list), None, redact)
                        }
                        None => (Some(sub_allow.iter().cloned().collect()), None, redact),
                    }
                }
                SubgraphConfig::Deny {
                    deny_extensions_keys: sub_deny,
                    common:
                        SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys,
                        },
                } => {
                    let redact = sub_redact.unwrap_or(default_config.redact_message);
                    match &default_config.deny_extensions_keys {
                        Some(global_deny) => {
                            let mut deny_list = global_deny.clone();
                            // Remove excluded keys from global
                            if let Some(exclude_keys) = exclude_global_keys {
                                deny_list.retain(|key| !exclude_keys.contains(key));
                            }
                            // Now merge sub_deny
                            deny_list.extend(sub_deny.clone());
                            (None, Some(deny_list), redact)
                        }
                        None => (None, Some(sub_deny.iter().cloned().collect()), redact),
                    }
                }
                SubgraphConfig::Included(enabled) => (
                    // Discard global allow/deny when subgraph is bool
                    None,
                    None,
                    if *enabled {
                        false // no redaction when subgraph is true
                    } else {
                        true // full redaction when subgraph is false
                    },
                ),
                SubgraphConfig::CommonOnly {
                    common:
                        SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys: _,
                        },
                } => {
                    let redact = sub_redact.unwrap_or(default_config.redact_message);
                    // Inherit global allow/deny lists when using CommonOnly
                    match config.all.clone() {
                        ErrorMode::Allow {
                            allow_extensions_keys,
                            ..
                        } => (
                            Some(allow_extensions_keys.iter().cloned().collect()),
                            None,
                            redact,
                        ),
                        ErrorMode::Deny {
                            deny_extensions_keys,
                            ..
                        } => (
                            None,
                            Some(deny_extensions_keys.iter().cloned().collect()),
                            redact,
                        ),
                        _ => (None, None, redact),
                    }
                }
            };

            effective_config.subgraphs.insert(
                name.clone(),
                SubgraphEffectiveConfig {
                    include_errors: effective_redact,
                    redact_message: false,
                    allow_extensions_keys: effective_allow,
                    deny_extensions_keys: effective_deny,
                },
            );
        }

        Ok(effective_config)
    }
}

impl EffectiveConfig {
    fn default_config(config: &Config) -> SubgraphEffectiveConfig {
        let (global_include_errors, global_redact_message, global_allow_keys, global_deny_keys) =
            match &config.all {
                ErrorMode::Included(enabled) => (*enabled, !*enabled, None, None), // Redact if not enabled
                ErrorMode::Allow {
                    allow_extensions_keys,
                    redact_message,
                } => (
                    true,
                    *redact_message,
                    Some(
                        allow_extensions_keys
                            .iter()
                            .cloned()
                            .collect::<HashSet<_>>(),
                    ),
                    None,
                ),
                ErrorMode::Deny {
                    deny_extensions_keys,
                    redact_message,
                } => (
                    true,
                    *redact_message,
                    None,
                    Some(deny_extensions_keys.iter().cloned().collect::<HashSet<_>>()),
                ),
            };
        let default_config = SubgraphEffectiveConfig {
            include_errors: global_include_errors,
            redact_message: global_redact_message,
            allow_extensions_keys: global_allow_keys.clone(),
            deny_extensions_keys: global_deny_keys.clone(),
        };
        default_config
    }
}
