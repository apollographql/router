//! Environment variable expansion in the configuration file
// This entire file is license key functionality

use std::env;
use std::env::VarError;
use std::fs;

use proteus::Parser;
use proteus::TransformBuilder;
use serde_json::Value;

use super::ConfigurationError;

#[derive(buildstructor::Builder)]
pub(crate) struct Expansion {
    prefix: Option<String>,
    supported_modes: Vec<String>,
    defaults: Vec<ConfigDefault>,
}

#[derive(buildstructor::Builder)]
pub(crate) struct ConfigDefault {
    config_path: String,
    env_name: String,
    default: String,
}

impl Expansion {
    pub(crate) fn default() -> Result<Self, ConfigurationError> {
        // APOLLO_ROUTER_CONFIG_SUPPORTED_MODES and APOLLO_ROUTER_CONFIG_SUPPORTED_MODES are unspported and may change in future.
        // If you need this functionality then raise an issue and we can look to promoting this to official support.
        let prefix = match env::var("APOLLO_ROUTER_CONFIG_ENV_PREFIX") {
            Ok(v) => Some(v),
            Err(VarError::NotPresent) => None,
            Err(VarError::NotUnicode(_)) => Err(ConfigurationError::InvalidExpansionModeConfig)?,
        };
        let supported_expansion_modes = match env::var("APOLLO_ROUTER_CONFIG_SUPPORTED_MODES") {
            Ok(v) => v,
            Err(VarError::NotPresent) => "env,file".to_string(),
            Err(VarError::NotUnicode(_)) => Err(ConfigurationError::InvalidExpansionModeConfig)?,
        };
        let supported_modes = supported_expansion_modes
            .split(',')
            .map(|mode| mode.trim().to_string())
            .collect::<Vec<String>>();
        Ok(Expansion::builder()
            .and_prefix(prefix)
            .supported_modes(supported_modes)
            .default(
                ConfigDefault::builder()
                    .config_path("telemetry.apollo.endpoint")
                    .env_name("APOLLO_USAGE_REPORTING_INGRESS_URL")
                    .default("https://usage-reporting.api.apollographql.com/api/ingress/traces")
                    .build(),
            )
            .build())
    }
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
                return match self.prefix.as_ref() {
                    None => env::var(key),
                    Some(prefix) => env::var(format!("{}_{}", prefix, key)),
                }
                .map(Some)
                .map_err(|cause| ConfigurationError::CannotExpandVariable {
                    key: key.to_string(),
                    cause: format!("{}", cause),
                });
            }
            if let Some(key) = key.strip_prefix("file.") {
                if !std::path::Path::new(key).exists() {
                    return Ok(None);
                }

                return fs::read_to_string(key).map(Some).map_err(|cause| {
                    ConfigurationError::CannotExpandVariable {
                        key: key.to_string(),
                        cause: format!("{}", cause),
                    }
                });
            }
            Err(ConfigurationError::InvalidExpansionModeConfig)
        }
    }

    pub(crate) fn expand_env_variables(
        &self,
        configuration: &serde_json::Value,
    ) -> Result<serde_json::Value, ConfigurationError> {
        let mut configuration = configuration.clone();
        self.env_defaults(&mut configuration)?;
        self.visit(&mut configuration)?;
        Ok(configuration)
    }

    fn env_defaults(&self, config: &mut Value) -> Result<(), ConfigurationError> {
        // Anything that needs expanding via env variable should be placed here. Don't pollute the codebase with calls to std::env.
        // For testing we have the one fixed expansion. We don't actually want to expand env variables during tests
        let mut transformer_builder = TransformBuilder::default();
        transformer_builder =
            transformer_builder.add_action(Parser::parse("", "").expect("migration must be valid"));
        for default in &self.defaults {
            let env_variable =
                std::env::var(&default.env_name).unwrap_or_else(|_| default.default.clone());
            if jsonpath_lib::select(config, &format!("$.{}", default.config_path))
                .unwrap_or_default()
                .is_empty()
            {
                transformer_builder = transformer_builder.add_action(
                    Parser::parse(
                        &format!("const(\"{}\")", env_variable),
                        &default.config_path,
                    )
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

    use crate::configuration::expansion::ConfigDefault;
    use crate::configuration::Expansion;

    #[test]
    fn test_unprefixed() {
        std::env::set_var("TEST_EXPANSION_VAR", "expanded");
        std::env::set_var("TEST_DEFAULTED_VAR", "defaulted");

        let expansion = Expansion::builder()
            .supported_mode("env")
            .default(
                ConfigDefault::builder()
                    .config_path("defaulted")
                    .env_name("TEST_DEFAULTED_VAR")
                    .default("defaulted")
                    .build(),
            )
            .default(
                ConfigDefault::builder()
                    .config_path("overridden")
                    .env_name("TEST_DEFAULTED_VAR")
                    .default("defaulted")
                    .build(),
            )
            .build();
        let mut value =
            json!({"expanded": "${env.TEST_EXPANSION_VAR}", "overridden": "overridden"});
        value = expansion
            .expand_env_variables(&value)
            .expect("expansion must succeed");
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }

    #[test]
    fn test_prefixed() {
        std::env::set_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded");
        std::env::set_var("TEST_DEFAULTED_VAR", "defaulted");

        let expansion = Expansion::builder()
            .prefix("TEST_PREFIX")
            .supported_mode("env")
            .default(
                ConfigDefault::builder()
                    .config_path("defaulted")
                    .env_name("TEST_DEFAULTED_VAR")
                    .default("defaulted")
                    .build(),
            )
            .default(
                ConfigDefault::builder()
                    .config_path("overridden")
                    .env_name("TEST_DEFAULTED_VAR")
                    .default("defaulted")
                    .build(),
            )
            .build();
        let mut value =
            json!({"expanded": "${env.TEST_EXPANSION_VAR}", "overridden": "overridden"});
        value = expansion
            .expand_env_variables(&value)
            .expect("expansion must succeed");
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }
}
