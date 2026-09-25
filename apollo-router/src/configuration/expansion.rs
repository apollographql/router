//! Environment variable expansion and external overrides for the configuration file

use std::collections::HashMap;
use std::env;
use std::env::VarError;
use std::str::FromStr;
use std::sync::atomic::Ordering;

use apollo_configuration::expansion::FileVariables;
use apollo_configuration::expansion::LookupError;
use apollo_configuration::expansion::VariableProvider;
use apollo_configuration::expansion::argument_for_kind;
use apollo_configuration::provenance::Injection;
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
    /// Applies the `--dev` config after parsing.
    dev_mode: Option<bool>,
    #[cfg(test)]
    mocked_env_vars: HashMap<String, String>,
}

#[derive(buildstructor::Builder, Clone)]
pub(crate) struct Override {
    /// The path to the config value to override.
    config_path: String,
    /// Env variables take precedence over the flag's value.
    env_name: Option<String>,
    /// A value supplied by a command-line flag, used when the environment variable is unset.
    flag_value: Option<FlagValue>,
    /// The type of the value, used to coerce env variables.
    value_type: ValueType,
    #[cfg(test)]
    mocked_env_vars: HashMap<String, String>,
}

/// A command-line flag and the value it supplies. The flag is named in diagnostics.
#[derive(Clone)]
pub(crate) struct FlagValue {
    flag: &'static str,
    value: Value,
}

impl FlagValue {
    pub(crate) fn new(flag: &'static str, value: impl Into<Value>) -> Self {
        Self {
            flag,
            value: value.into(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum ValueType {
    String,
    #[allow(dead_code)]
    Number,
    #[allow(dead_code)]
    Bool,
}

impl Override {
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
    fn injection(&self) -> Option<Injection> {
        let path: Vec<&str> = self.config_path.split('.').collect();
        if let (Some(value), Some(name)) = (self.env_value(), &self.env_name) {
            return Some(Injection::env(&path, value, name));
        }
        let FlagValue { flag, value } = self.flag_value.as_ref()?;
        Some(Injection::cli_flag(&path, value.clone(), flag))
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

        let dev_mode = crate::executable::APOLLO_ROUTER_DEV_MODE.load(Ordering::Relaxed);
        if dev_mode {
            tracing::info!(
                "Running with *development* mode settings which facilitate development experience (e.g., introspection enabled)"
            );
        }

        let builder = Expansion::builder();
        #[cfg(test)]
        let builder = builder.mocked_env_vars(mocked_env_vars);
        let listen = *crate::executable::APOLLO_ROUTER_LISTEN_ADDRESS.lock();
        let listen_override = Override::builder()
            .config_path("supergraph.listen")
            .and_flag_value(listen.map(|listen| FlagValue::new("--listen", listen.to_string())))
            .value_type(ValueType::String)
            .build();
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
            .dev_mode(dev_mode)
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

impl Expansion {
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
            .dev_mode(expansion.dev_mode.unwrap_or_default())
            .add_variables(UnsupportedMode { supported_modes })
            .inject(injections)
    }
}

/// Resolves `${env.NAME}`, reading `<prefix>_NAME` when the router has an environment prefix.
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

#[cfg(test)]
mod test {
    use apollo_configuration::provenance::Injection;
    use insta::assert_yaml_snapshot;
    use serde_json::Value;

    use crate::configuration::Expansion;
    use crate::configuration::apollo_configuration_parse::ExternalValues;
    use crate::configuration::expansion::FlagValue;
    use crate::configuration::expansion::Override;
    use crate::configuration::expansion::ValueType;

    /// Expands `yaml` with `expansion`'s providers and overrides, without Router's schema.
    fn expand(expansion: &Expansion, yaml: &str) -> Value {
        ExternalValues::from(expansion.clone())
            .expand_without_schema(yaml)
            .expect("expansion must succeed")
    }

    fn injection(override_config: Override) -> String {
        format!("{:?}", override_config.injection())
    }

    fn from_env(name: &str, value: Value) -> String {
        format!("{:?}", Some(Injection::env(&[""], value, name)))
    }

    fn from_flag(flag: &str, value: Value) -> String {
        format!("{:?}", Some(Injection::cli_flag(&[""], value, flag)))
    }

    #[test]
    fn test_override_precedence() {
        assert_eq!(
            "None",
            injection(
                Override::builder()
                    .mocked_env_var("TEST_OVERRIDE", "env_override")
                    .config_path("")
                    .value_type(ValueType::String)
                    .build()
            )
        );
        assert_eq!(
            "None",
            injection(
                Override::builder()
                    .mocked_env_var("TEST_OVERRIDE", "env_override")
                    .config_path("")
                    .env_name("NON_EXISTENT")
                    .value_type(ValueType::String)
                    .build()
            )
        );
        assert_eq!(
            from_flag("--listen", Value::String("override".to_string())),
            injection(
                Override::builder()
                    .mocked_env_var("TEST_OVERRIDE", "env_override")
                    .config_path("")
                    .env_name("NON_EXISTENT")
                    .flag_value(FlagValue::new("--listen", "override"))
                    .value_type(ValueType::String)
                    .build()
            )
        );
        assert_eq!(
            from_flag("--listen", Value::String("override".to_string())),
            injection(
                Override::builder()
                    .mocked_env_var("TEST_OVERRIDE", "env_override")
                    .config_path("")
                    .flag_value(FlagValue::new("--listen", "override"))
                    .value_type(ValueType::String)
                    .build()
            )
        );
        assert_eq!(
            from_env("TEST_OVERRIDE", Value::String("env_override".to_string())),
            injection(
                Override::builder()
                    .mocked_env_var("TEST_OVERRIDE", "env_override")
                    .config_path("")
                    .env_name("TEST_OVERRIDE")
                    .flag_value(FlagValue::new("--listen", "override"))
                    .value_type(ValueType::String)
                    .build()
            )
        );
    }

    #[test]
    fn test_type_coercion() {
        let coerced = |name: &str, value: &str, value_type: ValueType| {
            injection(
                Override::builder()
                    .mocked_env_var(name, value)
                    .config_path("")
                    .env_name(name)
                    .value_type(value_type)
                    .build(),
            )
        };
        assert_eq!(
            from_env(
                "TEST_DEFAULTED_STRING_VAR",
                Value::String("overridden_string".to_string())
            ),
            coerced(
                "TEST_DEFAULTED_STRING_VAR",
                "overridden_string",
                ValueType::String
            )
        );
        assert_eq!(
            from_env("TEST_DEFAULTED_NUMERIC_VAR", Value::Number(1.into())),
            coerced("TEST_DEFAULTED_NUMERIC_VAR", "1", ValueType::Number)
        );
        assert_eq!(
            from_env("TEST_DEFAULTED_BOOL_VAR", Value::Bool(true)),
            coerced("TEST_DEFAULTED_BOOL_VAR", "true", ValueType::Bool)
        );
        assert_eq!(
            from_env(
                "TEST_DEFAULTED_INCORRECT_TYPE",
                Value::String("true".to_string())
            ),
            coerced("TEST_DEFAULTED_INCORRECT_TYPE", "true", ValueType::Number)
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
                    .flag_value(FlagValue::new("--listen", "defaulted"))
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("no_env")
                    .env_name("NON_EXISTENT")
                    .flag_value(FlagValue::new("--listen", "defaulted"))
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("overridden")
                    .env_name("TEST_OVERRIDDEN_VAR")
                    .flag_value(FlagValue::new("--listen", "defaulted"))
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
                    .flag_value(FlagValue::new("--listen", "defaulted"))
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("no_env")
                    .env_name("NON_EXISTENT")
                    .flag_value(FlagValue::new("--listen", "defaulted"))
                    .value_type(ValueType::String)
                    .build(),
            )
            .override_config(
                Override::builder()
                    .mocked_env_var("TEST_PREFIX_TEST_EXPANSION_VAR", "expanded")
                    .mocked_env_var("TEST_OVERRIDDEN_VAR", "overridden")
                    .config_path("overridden")
                    .env_name("TEST_OVERRIDDEN_VAR")
                    .flag_value(FlagValue::new("--listen", "defaulted"))
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

    /// `--dev` config is applied after migration, so it wins over a legacy key that migration
    /// moves onto the same path, and it reaches the typed config, the plugin config and the
    /// retained document alike.
    #[test]
    fn dev_mode_applies_after_migration() {
        let expansion = Expansion::builder().dev_mode(true).build();

        let config = crate::configuration::parse_configuration(
            "cors:\n  origins:\n    - https://example.com\nexpose_query_plan: false\nhomepage:\n  enabled: true\n",
            expansion,
            crate::configuration::Migration::WithinMajor,
        )
        .expect("the migrated document is valid");

        assert!(config.supergraph.introspection);
        assert!(config.sandbox.enabled);
        assert!(!config.homepage.enabled);
        assert!(config.plugin_config("apollo.expose_query_plan").is_some());
        assert_eq!(config.apollo_plugins.plugins["expose_query_plan"], true);
        let document = config.validated_yaml.expect("the retained document");
        assert_eq!(document["expose_query_plan"], true);
        assert_eq!(document["sandbox"]["enabled"], true);
        assert_eq!(document["homepage"]["enabled"], false);
        assert_eq!(document["supergraph"]["introspection"], true);
        assert_eq!(
            document["telemetry"]["exporters"]["tracing"]["response_trace_id"]["enabled"],
            true
        );
        assert_eq!(
            document["cors"]["policies"][0]["origins"][0],
            "https://example.com"
        );
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

    /// `--dev` sets the sandbox, homepage and introspection settings, so a file whose own values
    /// for them conflict still loads with it, as in earlier releases, and fails without it.
    #[test]
    fn dev_mode_fixes_sandbox_settings_the_file_conflicts_on() {
        let text = "sandbox:\n  enabled: true\n";

        let config = crate::configuration::parse_configuration(
            text,
            Expansion::builder().dev_mode(true).build(),
            crate::configuration::Migration::WithinMajor,
        )
        .expect("--dev disables the homepage and enables introspection");
        assert!(config.sandbox.enabled);

        let error = crate::configuration::parse_configuration(
            text,
            Expansion::builder().build(),
            crate::configuration::Migration::WithinMajor,
        )
        .expect_err("the homepage is enabled by default")
        .to_string();
        assert!(
            error.contains("sandbox and homepage cannot be enabled"),
            "{error}"
        );
    }

    /// `--dev` replaces a section that has its own settings, as earlier releases did, instead of
    /// refusing to override it.
    #[test]
    fn dev_mode_replaces_a_section_with_settings() {
        let expansion = Expansion::builder().dev_mode(true).build();

        let config = crate::configuration::parse_configuration(
            "include_subgraph_errors:\n  all:\n    allow_extensions_keys: [code]\n    redact_message: true\n",
            expansion,
            crate::configuration::Migration::WithinMajor,
        )
        .expect("--dev replaces the section");

        assert_eq!(
            config.validated_yaml.expect("the retained document")["include_subgraph_errors"]["all"],
            serde_json::json!(true)
        );
    }

    /// `--dev` config is applied after parsing, so diagnostics quote the operator's file: a
    /// literal anchored into a secret field stays redacted and locations match the file.
    #[test]
    fn dev_mode_diagnostics_quote_the_file_with_anchors_redacted() {
        let expansion = Expansion::builder().dev_mode(true).build();
        let text = "# operator comment\napq:\n  router:\n    cache:\n      redis:\n        urls: [redis://localhost]\n        namespace: &pw anchored-secret-value\n        unexpected: true\n        password: *pw\n"; // gitleaks:allow

        let error = crate::configuration::parse_configuration(
            text,
            expansion,
            crate::configuration::Migration::WithinMajor,
        )
        .expect_err("the Redis configuration contains an unknown field")
        .to_string();

        assert!(!error.contains("anchored-secret-value"), "{error}");
        // Only the file has the anchor; a serialized copy would expand it.
        assert!(error.contains("namespace: &pw [REDACTED]"), "{error}");
        // The leading comment makes this line 6 of the file; a serialized copy has no comment.
        assert!(error.contains("[6:9]"), "{error}");
    }
}
