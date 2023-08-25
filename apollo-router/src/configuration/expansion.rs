//! Environment variable expansion in the configuration file

use std::env;
use std::env::VarError;
use std::fs;
use std::str::FromStr;

use proteus::Parser;
use proteus::TransformBuilder;
use serde_json::Value;

use super::ConfigurationError;
use crate::executable::APOLLO_ROUTER_DEV_ENV;

#[derive(buildstructor::Builder, Clone)]
pub(crate) struct Expansion {
    prefix: Option<String>,
    supported_modes: Vec<String>,
    override_configs: Vec<Override>,
}

#[derive(buildstructor::Builder, Clone)]
pub(crate) struct Override {
    /// The path to the config value to override.
    config_path: String,
    /// Env variables take precedence over any override values.
    env_name: Option<String>,
    /// Override value
    value: Option<Value>,
    /// The type of the value, used to coerce env variables.
    value_type: ValueType,
}

#[derive(Clone)]
pub(crate) enum ValueType {
    String,
    #[allow(dead_code)]
    Number,
    Bool,
}

impl Override {
    fn value(&self) -> Option<Value> {
        // Order of precedence is:
        // 1. If the env variable is set, use that
        // 2. If the override is set, use that
        // 3. Don't change the config
        match (
            self.env_name
                .as_ref()
                .and_then(|name| std::env::var(name).ok()),
            self.value.clone(),
        ) {
            (Some(value), _) => {
                // Coerce the env variable into the correct format, otherwise let it through as a string
                let parsed = Value::from_str(&value);
                let string_var = Value::String(value);
                Some(match (&self.value_type, parsed) {
                    (ValueType::Bool, Ok(Value::Bool(bool))) => Value::Bool(bool),
                    (ValueType::Number, Ok(Value::Number(number))) => Value::Number(number),
                    _ => string_var,
                })
            }
            (_, Some(value)) => Some(value),
            _ => None,
        }
    }
}

impl Expansion {
    pub(crate) fn default() -> Result<Self, ConfigurationError> {
        let prefix = Expansion::prefix_from_env()?;

        let supported_expansion_modes = match env::var("APOLLO_ROUTER_CONFIG_SUPPORTED_MODES") {
            Ok(v) => v,
            Err(VarError::NotPresent) => "env,file".to_string(),
            Err(VarError::NotUnicode(_)) => Err(ConfigurationError::InvalidExpansionModeConfig)?,
        };
        let supported_modes = supported_expansion_modes
            .split(',')
            .map(|mode| mode.trim().to_string())
            .collect::<Vec<String>>();

        let dev_mode_defaults = if std::env::var(APOLLO_ROUTER_DEV_ENV).ok().as_deref()
            == Some("true")
        {
            tracing::info!("Running with *development* mode settings which facilitate development experience (e.g., introspection enabled)");
            dev_mode_defaults()
        } else {
            Vec::new()
        };

        Ok(Expansion::builder()
            .and_prefix(prefix)
            .supported_modes(supported_modes)
            .override_config(
                Override::builder()
                    .config_path("telemetry.apollo.endpoint")
                    .env_name("APOLLO_USAGE_REPORTING_INGRESS_URL")
                    .value_type(ValueType::String)
                    .build(),
            )
            // Note that APOLLO_USAGE_REPORTING_OTLP_INGRESS_URL is experimental and subject to change without notice
            .override_config(
                Override::builder()
                    .config_path("telemetry.apollo.experimental_otlp_endpoint")
                    .env_name("APOLLO_USAGE_REPORTING_OTLP_INGRESS_URL")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .config_path("supergraph.listen")
                    .env_name("APOLLO_ROUTER_LISTEN_ADDRESS")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_configs(dev_mode_defaults)
            .build())
    }

    pub(crate) fn default_rhai() -> Result<Self, ConfigurationError> {
        Ok(Expansion::builder()
            .and_prefix(Expansion::prefix_from_env()?)
            .build())
    }

    fn prefix_from_env() -> Result<Option<String>, ConfigurationError> {
        // APOLLO_ROUTER_CONFIG_ENV_PREFIX and APOLLO_ROUTER_CONFIG_SUPPORTED_MODES are unsupported and may change in future.
        // If you need this functionality then raise an issue and we can look to promoting this to official support.
        match env::var("APOLLO_ROUTER_CONFIG_ENV_PREFIX") {
            Ok(v) => Ok(Some(v)),
            Err(VarError::NotPresent) => Ok(None),
            Err(VarError::NotUnicode(_)) => Err(ConfigurationError::InvalidExpansionModeConfig),
        }
    }
}

fn dev_mode_defaults() -> Vec<Override> {
    vec![
        Override::builder()
            .config_path("plugins.[\"experimental.expose_query_plan\"]")
            .value(true)
            .value_type(ValueType::Bool)
            .build(),
        Override::builder()
            .config_path("include_subgraph_errors.all")
            .value(true)
            .value_type(ValueType::Bool)
            .build(),
        Override::builder()
            .config_path("telemetry.tracing.experimental_response_trace_id.enabled")
            .value(true)
            .value_type(ValueType::Bool)
            .build(),
        Override::builder()
            .config_path("supergraph.introspection")
            .value(true)
            .value_type(ValueType::Bool)
            .build(),
        Override::builder()
            .config_path("sandbox.enabled")
            .value(true)
            .value_type(ValueType::Bool)
            .build(),
        Override::builder()
            .config_path("homepage.enabled")
            .value(false)
            .value_type(ValueType::Bool)
            .build(),
    ]
}

impl Expansion {
    fn context_fn(&self) -> impl Fn(&str) -> Result<Option<String>, ConfigurationError> + '_ {
        move |key: &str| {
            if !self
                .supported_modes
                .iter()
                .any(|prefix| key.starts_with(prefix.as_str()))
            {
                return Err(ConfigurationError::UnknownExpansionMode {
                    key: key.to_string(),
                    supported_modes: self.supported_modes.join("|"),
                });
            }

            if let Some(key) = key.strip_prefix("env.") {
                return self.expand_env(key);
            }
            if let Some(key) = key.strip_prefix("file.") {
                if !std::path::Path::new(key).exists() {
                    return Ok(None);
                }

                return fs::read_to_string(key).map(Some).map_err(|cause| {
                    ConfigurationError::CannotExpandVariable {
                        key: key.to_string(),
                        cause: format!("{cause}"),
                    }
                });
            }
            Err(ConfigurationError::InvalidExpansionModeConfig)
        }
    }

    pub(crate) fn expand_env(&self, key: &str) -> Result<Option<String>, ConfigurationError> {
        match self.prefix.as_ref() {
            None => env::var(key),
            Some(prefix) => env::var(format!("{prefix}_{key}")),
        }
        .map(Some)
        .map_err(|cause| ConfigurationError::CannotExpandVariable {
            key: key.to_string(),
            cause: format!("{cause}"),
        })
    }

    pub(crate) fn expand(
        &self,
        configuration: &serde_json::Value,
    ) -> Result<serde_json::Value, ConfigurationError> {
        let mut configuration = configuration.clone();
        self.defaults(&mut configuration)?;
        self.visit(&mut configuration)?;
        Ok(configuration)
    }

    fn defaults(&self, config: &mut Value) -> Result<(), ConfigurationError> {
        // Anything that needs expanding via env variable should be placed here. Don't pollute the codebase with calls to std::env.
        // For testing we have the one fixed expansion. We don't actually want to expand env variables during tests
        let mut transformer_builder = TransformBuilder::default();
        transformer_builder =
            transformer_builder.add_action(Parser::parse("", "").expect("migration must be valid"));
        for override_config in &self.override_configs {
            if let Some(value) = override_config.value() {
                transformer_builder = transformer_builder.add_action(
                    Parser::parse(&format!("const({value})"), &override_config.config_path)
                        .expect("migration must be valid"),
                );
            }
        }
        *config = transformer_builder
            .build()
            .expect("failed to build config default transformer")
            .apply(config)
            .map_err(|e| ConfigurationError::InvalidConfiguration {
                message: "could not set configuration defaults as the source configuration had an invalid structure",
                error: e.to_string(),
            })?;
        Ok(())
    }

    fn visit(&self, value: &mut Value) -> Result<(), ConfigurationError> {
        let mut expanded: Option<String> = None;
        match value {
            Value::String(value) => {
                let new_value =
                    shellexpand::env_with_context(value, self.context_fn()).map_err(|e| e.cause)?;
                if &new_value != value {
                    expanded = Some(new_value.to_string());
                }
            }
            Value::Array(a) => {
                for v in a {
                    self.visit(v)?
                }
            }
            Value::Object(o) => {
                for v in o.values_mut() {
                    self.visit(v)?
                }
            }
            _ => {}
        }
        // The expansion may have resulted in a primitive, reparse and replace
        if let Some(expanded) = expanded {
            *value = coerce(&expanded)
        }
        Ok(())
    }
}

pub(crate) fn coerce(expanded: &str) -> Value {
    match serde_yaml::from_str(expanded) {
        Ok(Value::Bool(b)) => Value::Bool(b),
        Ok(Value::Number(n)) => Value::Number(n),
        Ok(Value::Null) => Value::Null,
        _ => Value::String(expanded.to_string()),
    }
}

#[cfg(test)]
mod test {
    use insta::assert_yaml_snapshot;
    use serde_json::json;
    use serde_json::Value;

    use crate::configuration::expansion::dev_mode_defaults;
    use crate::configuration::expansion::Override;
    use crate::configuration::expansion::ValueType;
    use crate::configuration::Expansion;

    #[test]
    fn test_override_precedence() {
        std::env::set_var("TEST_OVERRIDE", "env_override");
        assert_eq!(
            None,
            Override::builder()
                .config_path("")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            None,
            Override::builder()
                .config_path("")
                .env_name("NON_EXISTENT")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::String("override".to_string())),
            Override::builder()
                .config_path("")
                .env_name("NON_EXISTENT")
                .value("override")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::String("override".to_string())),
            Override::builder()
                .config_path("")
                .value("override")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::String("env_override".to_string())),
            Override::builder()
                .config_path("")
                .env_name("TEST_OVERRIDE")
                .value("override")
                .value_type(ValueType::String)
                .build()
                .value()
        );
    }

    #[test]
    fn test_type_coercion() {
        std::env::set_var("TEST_DEFAULTED_STRING_VAR", "overridden_string");
        std::env::set_var("TEST_DEFAULTED_NUMERIC_VAR", "1");
        std::env::set_var("TEST_DEFAULTED_BOOL_VAR", "true");
        std::env::set_var("TEST_DEFAULTED_INCORRECT_TYPE", "true");

        assert_eq!(
            Some(Value::String("overridden_string".to_string())),
            Override::builder()
                .config_path("")
                .env_name("TEST_DEFAULTED_STRING_VAR")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::Number(1.into())),
            Override::builder()
                .config_path("")
                .env_name("TEST_DEFAULTED_NUMERIC_VAR")
                .value_type(ValueType::Number)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::Bool(true)),
            Override::builder()
                .config_path("")
                .env_name("TEST_DEFAULTED_BOOL_VAR")
                .value_type(ValueType::Bool)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::String("true".to_string())),
            Override::builder()
                .config_path("")
                .env_name("TEST_DEFAULTED_INCORRECT_TYPE")
                .value_type(ValueType::Number)
                .build()
                .value()
        );
    }

    #[test]
    fn test_unprefixed() {
        std::env::set_var("TEST_EXPANSION_VAR", "expanded");
        std::env::set_var("TEST_OVERRIDDEN_VAR", "overridden");

        let expansion = Expansion::builder()
            .supported_mode("env")
            .override_config(
                Override::builder()
                    .config_path("defaulted")
                    .env_name("TEST_DEFAULTED_VAR")
                    .value("defaulted")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .config_path("no_env")
                    .env_name("NON_EXISTENT")
                    .value("defaulted")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .config_path("overridden")
                    .env_name("TEST_OVERRIDDEN_VAR")
                    .value("defaulted")
                    .value_type(ValueType::String)
                    .build(),
            )
            .build();

        let mut value = json!({"expanded": "${env.TEST_EXPANSION_VAR}", "overridden": "default"});
        value = expansion.expand(&value).expect("expansion must succeed");
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }

    #[test]
    fn test_prefixed() {
        std::env::set_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded");
        std::env::set_var("TEST_OVERRIDDEN_VAR", "overridden");

        let expansion = Expansion::builder()
            .prefix("TEST_PREFIX")
            .supported_mode("env")
            .override_config(
                Override::builder()
                    .config_path("defaulted")
                    .env_name("TEST_DEFAULTED_VAR")
                    .value("defaulted")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .config_path("no_env")
                    .env_name("NON_EXISTENT")
                    .value("defaulted")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .config_path("overridden")
                    .env_name("TEST_OVERRIDDEN_VAR")
                    .value("defaulted")
                    .value_type(ValueType::String)
                    .build(),
            )
            .build();
        let mut value = json!({"expanded": "${env.TEST_EXPANSION_VAR}", "overridden": "default"});
        value = expansion.expand(&value).expect("expansion must succeed");
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }

    #[test]
    fn test_dev_mode() {
        let expansion = Expansion::builder()
            .override_configs(dev_mode_defaults())
            .build();
        let mut value =
            json!({"homepage": {"enabled": false, "some_other_config": "should remain"}});
        value = expansion.expand(&value).expect("expansion must succeed");
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }
}
