use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json_bytes::ByteString;
use tower::BoxError;
use tower::ServiceExt;

use crate::json_ext::Object;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::SubgraphResponse;

static REDACTED_ERROR_MESSAGE: &str = "Subgraph errors redacted";

register_plugin!("apollo", "include_subgraph_errors", IncludeSubgraphErrors);

/// Configuration for exposing errors that originate from subgraphs
#[derive(Clone, Debug, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct Config {
    /// Configuration for all subgraphs, and handles subgraph errors
    all: ErrorMode,

    /// Override default configuration for specific subgraphs
    subgraphs: HashMap<String, SubgraphConfig>,
}

#[derive(Clone, Debug, JsonSchema, Deserialize, Serialize)]
#[serde(untagged)]
enum ErrorMode {
    /// Propagate original error or redact everything
    Included(bool),
    /// Allow specific extension keys with required redact_message
    Allow {
        allow_extensions_keys: Vec<String>,
        redact_message: bool,
    },
    /// Deny specific extension keys with required redact_message
    Deny {
        deny_extensions_keys: Vec<String>,
        redact_message: bool,
    },
}

impl Default for ErrorMode {
    fn default() -> Self {
        ErrorMode::Included(false)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SubgraphConfigCommon {
    #[serde(skip_serializing_if = "Option::is_none")]
    redact_message: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_global_keys: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum SubgraphConfig {
    /// Enable or disable error inclusion for a subgraph
    Included(bool),
    /// Allow specific extension keys for a subgraph
    Allow {
        allow_extensions_keys: Vec<String>,
        #[serde(flatten)]
        common: SubgraphConfigCommon,
    },
    /// Deny specific extension keys for a subgraph
    Deny {
        deny_extensions_keys: Vec<String>,
        #[serde(flatten)]
        common: SubgraphConfigCommon,
    },
    CommonOnly {
        #[serde(flatten)]
        common: SubgraphConfigCommon,
    }
}

impl JsonSchema for SubgraphConfig {
    fn schema_name() -> String {
        "SubgraphConfig".to_string()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, ObjectValidation, Schema, SchemaObject};

        let bool_schema = Schema::Object(SchemaObject {
            instance_type: Some(schemars::schema::SingleOrVec::Single(Box::new(
                InstanceType::Boolean,
            ))),
            ..Default::default()
        });

        let common_props = {
            let mut props = schemars::Map::new();
            props.insert(
                "redact_message".to_string(),
                gen.subschema_for::<Option<bool>>(),
            );
            props.insert(
                "exclude_global_keys".to_string(),
                gen.subschema_for::<Option<Vec<String>>>(),
            );
            props
        };

        let allow_schema = Schema::Object(SchemaObject {
            instance_type: Some(schemars::schema::SingleOrVec::Single(Box::new(
                InstanceType::Object,
            ))),
            object: Some(Box::new(ObjectValidation {
                properties: {
                    let mut props = common_props.clone();
                    props.insert(
                        "allow_extensions_keys".to_string(),
                        gen.subschema_for::<Vec<String>>(),
                    );
                    props
                },
                required: vec!["allow_extensions_keys".to_string()]
                    .into_iter()
                    .collect(),
                additional_properties: Some(Box::new(Schema::Bool(false))),
                ..Default::default()
            })),
            ..Default::default()
        });

        let deny_schema = Schema::Object(SchemaObject {
            instance_type: Some(schemars::schema::SingleOrVec::Single(Box::new(
                InstanceType::Object,
            ))),
            object: Some(Box::new(ObjectValidation {
                properties: {
                    let mut props = common_props.clone();
                    props.insert(
                        "deny_extensions_keys".to_string(),
                        gen.subschema_for::<Vec<String>>(),
                    );
                    props
                },
                required: vec!["deny_extensions_keys".to_string()]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<String>>(),
                additional_properties: Some(Box::new(Schema::Bool(false))),
                ..Default::default()
            })),
            ..Default::default()
        });

        let common_schema = Schema::Object(SchemaObject {
            instance_type: Some(schemars::schema::SingleOrVec::Single(Box::new(
                InstanceType::Object,
            ))),
            object: Some(Box::new(ObjectValidation {
                properties: common_props.clone(),
                // Prevent additional properties
                additional_properties: Some(Box::new(Schema::Bool(false))),
                ..Default::default()
            })),
            ..Default::default()
        });

        Schema::Object(SchemaObject {
            subschemas: Some(Box::new(schemars::schema::SubschemaValidation {
                one_of: Some(vec![
                    bool_schema,
                    allow_schema,
                    deny_schema,
                    common_schema,
                ]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

// Custom deserializer to handle both boolean and object types
impl<'de> Deserialize<'de> for SubgraphConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};
        use std::fmt;

        struct SubgraphConfigVisitor;

        impl<'de> Visitor<'de> for SubgraphConfigVisitor {
            type Value = SubgraphConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "boolean or object with allow_extensions_keys or deny_extensions_keys",
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
                M: de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                #[serde(untagged)]
                enum Helper {
                    Allow {
                        allow_extensions_keys: Vec<String>,
                        #[serde(flatten)]
                        common: SubgraphConfigCommon,
                    },
                    Deny {
                        deny_extensions_keys: Vec<String>,
                        #[serde(flatten)]
                        common: SubgraphConfigCommon,
                    },
                    Common {
                        #[serde(flatten)]
                        common: SubgraphConfigCommon,
                    },
                }

                let helper: Helper =
                    Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;

                match helper {
                    Helper::Allow {
                        allow_extensions_keys,
                        common,
                    } => Ok(SubgraphConfig::Allow {
                        allow_extensions_keys,
                        common,
                    }),
                    Helper::Deny {
                        deny_extensions_keys,
                        common,
                    } => Ok(SubgraphConfig::Deny {
                        deny_extensions_keys,
                        common,
                    }),
                    Helper::Common { common } => Ok(SubgraphConfig::CommonOnly { common }),
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

struct IncludeSubgraphErrors {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for IncludeSubgraphErrors {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        // Validate configuration based on global mode
        match &init.config.all {
            ErrorMode::Included(_) => {
                // When global uses boolean, subgraph configs must be boolean.
                for (name, config) in &init.config.subgraphs {
                    match config {
                        SubgraphConfig::Included(_) => {},
                        _ => {
                            return Err(format!(
                                "Subgraph '{}' must use boolean config when global config is boolean or not present",
                                name
                            ).into());
                        }
                    }
                }
            }
            // When global is object (Allow or Deny), subgraphs can use bool, allow, or deny.
            // Single subgraph config can't have both allow and deny.
            _ => {}
        }

        Ok(IncludeSubgraphErrors {
            config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let subgraph_config = self.config.subgraphs.get(name).cloned();

        let (global_enabled, global_allow, global_deny, should_redact_message) =
            match self.config.all.clone() {
                ErrorMode::Allow {
                    allow_extensions_keys,
                    redact_message,
                } => (true, Some(allow_extensions_keys), None, redact_message),
                ErrorMode::Deny {
                    deny_extensions_keys,
                    redact_message,
                } => (true, None, Some(deny_extensions_keys), redact_message),
                // Set should_redact_message to true when enabled is false
                ErrorMode::Included(enabled) => (enabled, None, None, !enabled),
            };

        // Determine if we should include errors based on subgraph override or global setting
        let include_subgraph_errors = match &subgraph_config {
            Some(SubgraphConfig::Included(enabled)) => *enabled,
            Some(SubgraphConfig::Allow { .. }) => true,
            Some(SubgraphConfig::Deny { .. }) => true,
            Some(SubgraphConfig::CommonOnly { .. }) => true,
            None => global_enabled,
        };

        // Compute effective configuration by merging global and subgraph settings.
        let (effective_allow, effective_deny, effective_redact) =
            if let Some(ref sub_config) = subgraph_config {
                match sub_config {
                    SubgraphConfig::Allow {
                        allow_extensions_keys: sub_allow,
                        common: SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys,
                        },
                    } => {
                        let redact = sub_redact.unwrap_or(should_redact_message);
                        match &global_allow {
                            Some(global_allow) => {
                                let mut allow_list = global_allow.clone();

                                // Remove any keys that should be overridden
                                if let Some(exclude_keys) = exclude_global_keys {
                                    allow_list.retain(|key| !exclude_keys.contains(key));
                                }

                                // Add subgraph's allow keys
                                allow_list.extend(sub_allow.iter().cloned());
                                allow_list.sort();
                                allow_list.dedup();

                                (Some(allow_list), None, redact)
                            }
                            None => (Some(sub_allow.clone()), None, redact),
                        }
                    }
                    SubgraphConfig::Deny {
                        deny_extensions_keys: sub_deny,
                        common: SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys,
                        },
                    } => {
                        let redact = sub_redact.unwrap_or(should_redact_message);
                        match &global_deny {
                            Some(global_deny) => {
                                let mut deny_list = global_deny.clone();
                                // Remove excluded keys from global
                                if let Some(exclude_keys) = exclude_global_keys {
                                    deny_list.retain(|key| !exclude_keys.contains(key));
                                }
                                // Now merge sub_deny
                                deny_list.extend(sub_deny.clone());
                                deny_list.sort();
                                deny_list.dedup();
                                (None, Some(deny_list), redact)
                            }
                            None => (None, Some(sub_deny.clone()), redact),
                        }
                    }
                    SubgraphConfig::Included(enabled) => (
                        // Discard global allow/deny when subgraph is bool
                        None,
                        None,
                        if *enabled {
                            false // no redaction when subgraph is true
                        } else {
                            true  // full redaction when subgraph is false
                        },
                    ),
                    SubgraphConfig::CommonOnly {
                        common: SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys: _,
                        },
                    } => {
                        let redact = sub_redact.unwrap_or(should_redact_message);
                        (None, None, redact)
                    }
                }
            } else {
                match self.config.all.clone() {
                    ErrorMode::Allow {
                        allow_extensions_keys,
                        redact_message,
                    } => (Some(allow_extensions_keys), None, redact_message),
                    ErrorMode::Deny {
                        deny_extensions_keys,
                        redact_message,
                    } => (None, Some(deny_extensions_keys), redact_message),
                    ErrorMode::Included(_) => (None, None, should_redact_message),
                }
            };

        let sub_name_response = name.to_string();
        let sub_name_error = name.to_string();
        service
            .map_response(move |mut response: SubgraphResponse| {
                let errors = &mut response.response.body_mut().errors;
                if !errors.is_empty() {
                    if !include_subgraph_errors {
                        tracing::info!(
                            "redacted subgraph({sub_name_response}) errors - subgraph config"
                        );
                        // Redact based on subgraph config
                        for error in response.response.body_mut().errors.iter_mut() {
                            if effective_redact {
                                error.message = REDACTED_ERROR_MESSAGE.to_string();
                            }
                            // Remove all extensions unless they appear in effective_allow
                            let mut new_extensions = Object::new();
                            if let Some(allow_keys) = &effective_allow {
                                for key in allow_keys {
                                    if let Some(value) = error.extensions.get(key.as_str()) {
                                        new_extensions
                                            .insert(ByteString::from(key.clone()), value.clone());
                                    }
                                }
                            }
                            error.extensions = new_extensions;
                        }
                        return response;
                    }

                    for error in errors.iter_mut() {
                        // Handle message redaction based on effective_redact flag
                        if effective_redact {
                            error.message = REDACTED_ERROR_MESSAGE.to_string();
                        }

                        // Always include service name

                        // Filter extensions based on effective_allow if specified
                        if let Some(allow_keys) = &effective_allow {
                            let mut new_extensions = Object::new();
                            for key in allow_keys {
                                if let Some(value) = error.extensions.get(key.as_str()) {
                                    new_extensions
                                        .insert(ByteString::from(key.clone()), value.clone());
                                }
                            }
                            error.extensions = new_extensions;
                        }

                        // Remove extensions based on effective_deny if specified
                        if let Some(deny_keys) = &effective_deny {
                            for key in deny_keys {
                                error.extensions.remove(key.as_str());
                            }
                        }
                    }
                }

                response
            })
            .map_err(move |error: BoxError| {
                if include_subgraph_errors {
                    error
                } else {
                    // Create a redacted error to replace whatever error we have
                    tracing::info!("redacted subgraph({sub_name_error}) error");
                    let reason = if effective_redact {
                        "redacted".to_string()
                    } else {
                        error.to_string()
                    };
                    Box::new(crate::error::FetchError::SubrequestHttpError {
                        status_code: None,
                        service: "redacted".to_string(),
                        reason,
                    })
                }
            })
            .boxed()
    }
}
