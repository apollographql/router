//! Environment variable expansion and external overrides for the configuration file

use std::collections::HashMap;
use std::env;
use std::env::VarError;
use std::fs;
use std::str::FromStr;
use std::sync::atomic::Ordering;

use apollo_configuration::expansion::FileVariables;
use apollo_configuration::expansion::LookupError;
use apollo_configuration::expansion::VariableProvider;
use apollo_configuration::expansion::argument_for_kind;
use apollo_configuration::provenance::Injection;
use proteus::Parser;
use proteus::TransformBuilder;
use serde_json::Value;

use super::ConfigurationError;
use super::apollo_configuration_parse::ExternalValues;

/// The inputs the router supplies to the shared parser alongside the configuration file:
/// providers for `${env.NAME}` and `${file.PATH}` references, and overrides that set values at
/// fixed paths from environment variables and command-line flags.
#[derive(buildstructor::Builder, Clone)]
pub(crate) struct Expansion {
    prefix: Option<String>,
    supported_modes: Vec<String>,
    override_configs: Vec<Override>,
    #[cfg(test)]
    mocked_env_vars: HashMap<String, String>,
}

#[derive(buildstructor::Builder, Clone)]
pub(crate) struct Override {
    /// The path to the config value to override.
    config_path: String,
    /// Env variables take precedence over any override values.
    env_name: Option<String>,
    /// Override value
    value: Option<Value>,
    /// The command-line flag that supplies `value`, named in diagnostics.
    #[allow(dead_code)]
    flag: Option<String>,
    /// The type of the value, used to coerce env variables.
    value_type: ValueType,
    #[cfg(test)]
    mocked_env_vars: HashMap<String, String>,
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
        self.env_value().or_else(|| self.value.clone())
    }

    /// The environment variable's value, coerced to the override's type when it parses as one.
    fn env_value(&self) -> Option<Value> {
        let name = self.env_name.as_ref()?;
        #[cfg(test)]
        let value = self
            .mocked_env_vars
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok());
        #[cfg(not(test))]
        let value = std::env::var(name).ok();
        let value = value?;
        Some(match (&self.value_type, Value::from_str(&value)) {
            (ValueType::Bool, Ok(Value::Bool(bool))) => Value::Bool(bool),
            (ValueType::Number, Ok(Value::Number(number))) => Value::Number(number),
            _ => Value::String(value),
        })
    }

    /// The override as a shared-parser injection naming its source, when it supplies a value.
    #[allow(dead_code)]
    fn injection(&self) -> Option<Injection> {
        let path: Vec<&str> = self.config_path.split('.').collect();
        if let (Some(value), Some(name)) = (self.env_value(), &self.env_name) {
            return Some(Injection::env(&path, value, name));
        }
        let value = self.value.clone()?;
        let flag = self
            .flag
            .as_deref()
            .expect("an override with a fixed value names the flag that supplies it");
        Some(Injection::cli_flag(&path, value, flag))
    }
}

#[buildstructor::buildstructor]
impl Expansion {
    pub(crate) fn default() -> Result<Self, ConfigurationError> {
        Self::default_builder().build()
    }

    #[builder]
    pub(crate) fn default_new(
        #[cfg_attr(not(test), allow(unused))] mocked_env_vars: HashMap<String, String>,
    ) -> Result<Self, ConfigurationError> {
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

        let dev_mode_defaults = if crate::executable::APOLLO_ROUTER_DEV_MODE.load(Ordering::Relaxed)
        {
            tracing::info!(
                "Running with *development* mode settings which facilitate development experience (e.g., introspection enabled)"
            );
            dev_mode_defaults()
        } else {
            Vec::new()
        };

        let builder = Expansion::builder();
        #[cfg(test)]
        let builder = builder.mocked_env_vars(mocked_env_vars);
        let listen_override = Override::builder()
            .config_path("supergraph.listen")
            .flag("--listen")
            .value_type(ValueType::String);
        let listen = *crate::executable::APOLLO_ROUTER_LISTEN_ADDRESS.lock();
        let listen_override = if let Some(listen) = listen {
            listen_override.value(listen.to_string()).build()
        } else {
            listen_override.build()
        };
        Ok(builder
            .and_prefix(prefix)
            .supported_modes(supported_modes)
            .override_config(
                Override::builder()
                    .config_path("telemetry.apollo.endpoint")
                    .env_name("APOLLO_USAGE_REPORTING_INGRESS_URL")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .config_path("telemetry.apollo.otlp_endpoint")
                    .env_name("APOLLO_USAGE_REPORTING_OTLP_INGRESS_URL")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(listen_override)
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
    [
        "expose_query_plan",
        "include_subgraph_errors.all",
        "telemetry.exporters.tracing.response_trace_id.enabled",
        "supergraph.introspection",
        "sandbox.enabled",
        "connectors.debug_extensions",
    ]
    .into_iter()
    .map(|path| (path, true))
    .chain([("homepage.enabled", false)])
    .map(|(path, value)| {
        Override::builder()
            .config_path(path)
            .value(value)
            .flag("--dev")
            .value_type(ValueType::Bool)
            .build()
    })
    .collect()
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
        self.get_env(&self.env_name(key))
            .map(Some)
            .map_err(|cause| ConfigurationError::CannotExpandVariable {
                key: key.to_string(),
                cause: format!("{cause}"),
            })
    }

    fn env_name(&self, key: &str) -> String {
        match self.prefix.as_ref() {
            None => key.to_string(),
            Some(prefix) => format!("{prefix}_{key}"),
        }
    }

    fn get_env(&self, name: &str) -> Result<String, std::env::VarError> {
        #[cfg(test)]
        if let Some(value) = self.mocked_env_vars.get(name) {
            return Ok(value.clone());
        }
        env::var(name)
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
        transformer_builder = transformer_builder.add_action(Parser::parse("", "")?);
        for override_config in &self.override_configs {
            if let Some(value) = override_config.value() {
                transformer_builder = transformer_builder.add_action(Parser::parse(
                    &format!("const({value})"),
                    &override_config.config_path,
                )?);
            }
        }
        *config = transformer_builder
            .build()?
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

/// The router's expansion providers and overrides. Only the supported modes are expanded; any
/// other `${kind.NAME}` reference is an error.
impl From<Expansion> for ExternalValues {
    fn from(expansion: Expansion) -> Self {
        let injections: Vec<Injection> = expansion
            .override_configs
            .iter()
            .filter_map(Override::injection)
            .collect();
        let supported_modes = expansion.supported_modes.join("|");
        let mut external = ExternalValues::default();
        for mode in &expansion.supported_modes {
            external = match mode.as_str() {
                "env" => external.add_variables(EnvVariables(expansion.clone())),
                // Removes one trailing newline, so `true\n` in a file still expands to a boolean.
                "file" => external.add_variables(FileVariables),
                _ => external,
            };
        }
        external
            .add_variables(UnsupportedMode { supported_modes })
            .inject(injections)
    }
}

/// Resolves `${env.NAME}`, reading `<prefix>_NAME` when the router has an environment prefix.
#[allow(dead_code)]
struct EnvVariables(Expansion);

impl VariableProvider for EnvVariables {
    fn get(&self, reference: &str) -> Result<String, LookupError> {
        let key = argument_for_kind(reference, "env")?;
        self.0
            .get_env(&self.0.env_name(key))
            .map_err(|error| match error {
                VarError::NotPresent => LookupError::NotPresent,
                VarError::NotUnicode(_) => LookupError::NotUnicode,
            })
    }
}

/// Rejects references whose kind is not a supported mode.
#[allow(dead_code)]
struct UnsupportedMode {
    supported_modes: String,
}

impl VariableProvider for UnsupportedMode {
    fn get(&self, _reference: &str) -> Result<String, LookupError> {
        Err(LookupError::Other(format!(
            "variables must be prefixed with one of '{}' followed by '.' e.g. 'env.'",
            self.supported_modes
        )))
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
    use serde_json::Value;

    use crate::configuration::Expansion;
    use crate::configuration::apollo_configuration_parse::ExternalValues;
    use crate::configuration::expansion::Override;
    use crate::configuration::expansion::ValueType;
    use crate::configuration::expansion::dev_mode_defaults;

    /// Expands `yaml` with `expansion`'s providers and overrides, without Router's schema.
    fn expand(expansion: &Expansion, yaml: &str) -> Value {
        ExternalValues::from(expansion.clone())
            .expand_without_schema(yaml)
            .expect("expansion must succeed")
    }

    #[test]
    fn test_override_precedence() {
        assert_eq!(
            None,
            Override::builder()
                .mocked_env_var("TEST_OVERRIDE", "env_override")
                .config_path("")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            None,
            Override::builder()
                .mocked_env_var("TEST_OVERRIDE", "env_override")
                .config_path("")
                .env_name("NON_EXISTENT")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::String("override".to_string())),
            Override::builder()
                .mocked_env_var("TEST_OVERRIDE", "env_override")
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
                .mocked_env_var("TEST_OVERRIDE", "env_override")
                .config_path("")
                .value("override")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::String("env_override".to_string())),
            Override::builder()
                .mocked_env_var("TEST_OVERRIDE", "env_override")
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
        assert_eq!(
            Some(Value::String("overridden_string".to_string())),
            Override::builder()
                .mocked_env_var("TEST_DEFAULTED_STRING_VAR", "overridden_string")
                .config_path("")
                .env_name("TEST_DEFAULTED_STRING_VAR")
                .value_type(ValueType::String)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::Number(1.into())),
            Override::builder()
                .mocked_env_var("TEST_DEFAULTED_NUMERIC_VAR", "1")
                .config_path("")
                .env_name("TEST_DEFAULTED_NUMERIC_VAR")
                .value_type(ValueType::Number)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::Bool(true)),
            Override::builder()
                .mocked_env_var("TEST_DEFAULTED_BOOL_VAR", "true")
                .config_path("")
                .env_name("TEST_DEFAULTED_BOOL_VAR")
                .value_type(ValueType::Bool)
                .build()
                .value()
        );
        assert_eq!(
            Some(Value::String("true".to_string())),
            Override::builder()
                .mocked_env_var("TEST_DEFAULTED_INCORRECT_TYPE", "true")
                .config_path("")
                .env_name("TEST_DEFAULTED_INCORRECT_TYPE")
                .value_type(ValueType::Number)
                .build()
                .value()
        );
    }

    #[test]
    fn test_unprefixed() {
        let expansion = Expansion::builder()
            .mocked_env_var("TEST_EXPANSION_VAR", "expanded")
            .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
            .supported_mode("env")
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("defaulted")
                    .env_name("TEST_DEFAULTED_VAR")
                    .value("defaulted")
                    .flag("--test-flag")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("no_env")
                    .env_name("NON_EXISTENT")
                    .value("defaulted")
                    .flag("--test-flag")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("overridden")
                    .env_name("TEST_OVERRIDDEN_VAR")
                    .value("defaulted")
                    .flag("--test-flag")
                    .value_type(ValueType::String)
                    .build(),
            )
            .build();

        let value = expand(
            &expansion,
            "expanded: ${env.TEST_EXPANSION_VAR}\noverridden: default\n",
        );
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }

    #[test]
    fn test_prefixed() {
        let expansion = Expansion::builder()
            .mocked_env_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded")
            .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
            .prefix("TEST_PREFIX")
            .supported_mode("env")
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("defaulted")
                    .env_name("TEST_DEFAULTED_VAR")
                    .value("defaulted")
                    .flag("--test-flag")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("no_env")
                    .env_name("NON_EXISTENT")
                    .value("defaulted")
                    .flag("--test-flag")
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("overridden")
                    .env_name("TEST_OVERRIDDEN_VAR")
                    .value("defaulted")
                    .flag("--test-flag")
                    .value_type(ValueType::String)
                    .build(),
            )
            .build();
        let value = expand(
            &expansion,
            "expanded: ${env.TEST_EXPANSION_VAR}\noverridden: default\n",
        );
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }

    #[test]
    fn test_dev_mode() {
        let expansion = Expansion::builder()
            .override_configs(dev_mode_defaults())
            .build();
        let value = expand(
            &expansion,
            "homepage:\n  enabled: false\n  some_other_config: should remain\n",
        );
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(value);
        })
    }

    #[test]
    fn test_dollar_escape() {
        // Test that $$ is escaped to a literal $ by shellexpand
        let expansion = Expansion::builder()
            .mocked_env_var("API_HOST", "api.example.com")
            .supported_mode("env")
            .build();

        let result = expand(
            &expansion,
            r#"
# $$ should become a single $
literal_dollar: "some$$api$$key"
# $$ alongside ${env.VAR} expansion
mixed: "https://${env.API_HOST}/path?price=$$100"
# Multiple $$ in a row
multiple_escapes: "$$first $$second $$third"
# $$ at start and end
edges: "$$start and end$$"
# No expansion needed (plain string)
plain: "no dollars here"
"#,
        );
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(result);
        })
    }
}
