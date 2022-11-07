//! Environment variable expansion in the configuration file
// This entire file is license key functionality

use std::env;
use std::env::VarError;
use std::fs;

use serde_json::Value;

use super::ConfigurationError;

#[derive(buildstructor::Builder)]
pub(crate) struct Expansion {
    prefix: Option<String>,
    supported_modes: Vec<String>,
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
        Ok(Expansion {
            prefix,
            supported_modes,
        })
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
}

pub(crate) fn expand_env_variables(
    configuration: &serde_json::Value,
    expansion: Expansion,
) -> Result<serde_json::Value, ConfigurationError> {
    let mut configuration = configuration.clone();
    visit(&mut configuration, &expansion)?;
    Ok(configuration)
}

fn visit(value: &mut Value, expansion: &Expansion) -> Result<(), ConfigurationError> {
    let mut expanded: Option<String> = None;
    match value {
        Value::String(value) => {
            let new_value = shellexpand::env_with_context(value, expansion.context_fn())
                .map_err(|e| e.cause)?;
            if &new_value != value {
                expanded = Some(new_value.to_string());
            }
        }
        Value::Array(a) => {
            for v in a {
                visit(v, expansion)?
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                visit(v, expansion)?
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

pub(crate) fn coerce(expanded: &str) -> Value {
    match serde_yaml::from_str(expanded) {
        Ok(Value::Bool(b)) => Value::Bool(b),
        Ok(Value::Number(n)) => Value::Number(n),
        Ok(Value::Null) => Value::Null,
        _ => Value::String(expanded.to_string()),
    }
}
