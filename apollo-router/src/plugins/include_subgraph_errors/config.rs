use std::collections::HashMap;
use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Deserializer;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::de::{self};

/// Configuration for exposing errors that originate from subgraphs
#[derive(Clone, Debug, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
#[schemars(rename = "IncludeSubgraphErrorsConfig")]
pub(crate) struct Config {
    /// Global configuration for error redaction. Applies to all subgraphs.
    pub(crate) all: ErrorMode,

    /// Overrides global configuration on a per-subgraph basis
    pub(crate) subgraphs: HashMap<String, SubgraphConfig>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(untagged)]
pub(crate) enum ErrorMode {
    /// When `true`, Propagate the original error as is. Otherwise, redact it.
    Included(bool),
    /// Allow specific extension keys with required redact_message
    Allow {
        /// Allow specific extension keys
        allow_extensions_keys: Vec<String>,
        /// redact error messages for all subgraphs
        redact_message: bool,
    },
    /// Deny specific extension keys with required redact_message
    Deny {
        /// Deny specific extension keys
        deny_extensions_keys: Vec<String>,
        /// redact error messages for all subgraphs
        redact_message: bool,
    },
}

impl Default for ErrorMode {
    fn default() -> Self {
        ErrorMode::Included(false)
    }
}

impl<'de> Deserialize<'de> for ErrorMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ErrorModeVisitor;

        impl<'de> Visitor<'de> for ErrorModeVisitor {
            type Value = ErrorMode;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter
                    .write_str("boolean or object with allow_extensions_keys/deny_extensions_keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<ErrorMode, E>
            where
                E: de::Error,
            {
                Ok(ErrorMode::Included(value))
            }

            fn visit_map<M>(self, map: M) -> Result<ErrorMode, M::Error>
            where
                M: MapAccess<'de>,
            {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Helper {
                    allow_extensions_keys: Option<Vec<String>>,
                    deny_extensions_keys: Option<Vec<String>>,
                    redact_message: bool,
                }

                let helper = Helper::deserialize(de::value::MapAccessDeserializer::new(map))?;

                match (helper.allow_extensions_keys, helper.deny_extensions_keys) {
                    (Some(_), Some(_)) => Err(de::Error::custom(
                        "Global config cannot have both allow_extensions_keys and deny_extensions_keys",
                    )),
                    (Some(allow), None) => Ok(ErrorMode::Allow {
                        allow_extensions_keys: allow,
                        redact_message: helper.redact_message,
                    }),
                    (None, Some(deny)) => Ok(ErrorMode::Deny {
                        deny_extensions_keys: deny,
                        redact_message: helper.redact_message,
                    }),
                    // If neither allow nor deny is present, but redact_message is,
                    // treat it as Included(true) with the specified redaction.
                    // However, the current logic implies Included(true) means no redaction.
                    // Let's stick to the original logic: if neither list is present, it's Included(true).
                    // The `redact_message` field is only relevant for Allow/Deny variants here.
                    // If the user provides *only* `redact_message: bool`, it might be confusing.
                    // The original code defaults to Included(true) if neither key is present.
                    (None, None) => Ok(ErrorMode::Included(true)),
                }
            }
        }

        deserializer.deserialize_any(ErrorModeVisitor)
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(untagged)]
pub(crate) enum SubgraphConfig {
    /// Enable or disable error redaction for a subgraph
    Included(bool),
    /// Allow specific error extension keys for a subgraph
    Allow {
        /// Allow specific extension keys for a subgraph. Will extending global allow list or override a global deny list
        allow_extensions_keys: Vec<String>,
        /// Redact error messages for a subgraph
        #[serde(skip_serializing_if = "Option::is_none")]
        redact_message: Option<bool>,
        /// Exclude specific extension keys from global allow/deny list
        #[serde(default)]
        exclude_global_keys: Vec<String>,
    },
    /// Deny specific error extension keys for a subgraph
    Deny {
        /// Allow specific extension keys for a subgraph. Will extending global deny list or override a global allow list
        deny_extensions_keys: Vec<String>,
        /// Redact error messages for a subgraph
        #[serde(skip_serializing_if = "Option::is_none")]
        redact_message: Option<bool>,
        /// Exclude specific extension keys from global allow/deny list
        #[serde(default)]
        exclude_global_keys: Vec<String>,
    },
    /// Override global configuration, but don't allow or deny any new keys explicitly
    CommonOnly {
        /// Redact error messages for a subgraph
        #[serde(skip_serializing_if = "Option::is_none")]
        redact_message: Option<bool>,
        /// Exclude specific extension keys from global allow/deny list
        #[serde(default)]
        exclude_global_keys: Vec<String>,
    },
}

// Custom deserializer to handle both boolean and object types
impl<'de> Deserialize<'de> for SubgraphConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SubgraphConfigVisitor;

        impl<'de> Visitor<'de> for SubgraphConfigVisitor {
            type Value = SubgraphConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "boolean or object with optional allow_extensions_keys or deny_extensions_keys",
                )
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(SubgraphConfig::Included(value))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                // Intermediate struct to capture all possible fields
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct FullConfig {
                    allow_extensions_keys: Option<Vec<String>>,
                    deny_extensions_keys: Option<Vec<String>>,
                    redact_message: Option<bool>,
                    #[serde(default)]
                    exclude_global_keys: Vec<String>,
                }

                let config: FullConfig =
                    Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;

                match (config.allow_extensions_keys, config.deny_extensions_keys) {
                    (Some(_), Some(_)) => Err(de::Error::custom(
                        "A subgraph config cannot have both allow_extensions_keys and deny_extensions_keys",
                    )),
                    (Some(allow), None) => Ok(SubgraphConfig::Allow {
                        allow_extensions_keys: allow,
                        redact_message: config.redact_message,
                        exclude_global_keys: config.exclude_global_keys,
                    }),
                    (None, Some(deny)) => Ok(SubgraphConfig::Deny {
                        deny_extensions_keys: deny,
                        redact_message: config.redact_message,
                        exclude_global_keys: config.exclude_global_keys,
                    }),
                    (None, None) => {
                        // If neither allow nor deny keys are present, it's CommonOnly
                        Ok(SubgraphConfig::CommonOnly {
                            redact_message: config.redact_message,
                            exclude_global_keys: config.exclude_global_keys,
                        })
                    }
                }
            }
        }

        deserializer.deserialize_any(SubgraphConfigVisitor)
    }
}

impl Default for SubgraphConfig {
    fn default() -> Self {
        SubgraphConfig::Included(false)
    }
}
