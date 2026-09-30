//! Parses router configuration with `apollo-configuration`.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::OnceLock;

use apollo_configuration::ConfigError;
use apollo_configuration::ConfigParser;
use apollo_configuration::ErrorCollector;
use apollo_configuration::Validate;
use apollo_configuration::expansion::LookupError;
use apollo_configuration::expansion::VariableProvider;
use apollo_configuration::provenance::Injection;
use parking_lot::Mutex;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde_json::Value;
use serde_json::json;

use super::Configuration;
use super::ConfigurationError;
use super::expansion::Expansion;
use super::schema::router_config_schema;
use super::upgrade::UpgradeMode;
use super::upgrade::upgrade_configuration;
use super::upgrade::upgrade_configuration_silently;

/// Runs every plugin's validation rules, for a configuration assembled in code. Parsing runs
/// them through `ParsedConfiguration` instead, which knows which sections the document has.
impl Validate for Configuration {
    fn validate<'a>(&self, errors: ErrorCollector<'a>) {
        validate_plugin_sections(self, errors, |_| true);
    }
}

impl apollo_configuration::Configuration for Configuration {}

/// Runs the validation rules of each plugin section, at its path in the document: built-in
/// sections at their top-level key and user plugins under `plugins`. Built-in sections that
/// `in_document` rejects are skipped, as they have no location to report at.
fn validate_plugin_sections(
    config: &Configuration,
    mut errors: ErrorCollector<'_>,
    in_document: impl Fn(&str) -> bool,
) {
    for (name, parsed) in config.apollo_plugins.iter() {
        if in_document(name) {
            parsed.config.validate(errors.nest(name));
        }
    }
    let mut user = errors.nest("plugins");
    for (name, parsed) in config.plugins.iter() {
        parsed.config.validate(user.nest(name));
    }
}

/// Checks `value`'s validation rules outside a parse, so errors have no location.
#[cfg(any(test, feature = "mock_subgraphs_testing"))]
pub(crate) fn check_plugin_rules(value: &impl Validate) -> Result<(), ConfigurationError> {
    struct ByRef<'v, V>(&'v V);

    impl<V: Validate> Validate for ByRef<'_, V> {
        fn validate<'a>(&self, errors: ErrorCollector<'a>) {
            self.0.validate(errors)
        }
    }

    apollo_configuration::validate(ByRef(value))
        .map(|_| ())
        .map_err(|errors| ConfigError::ValidationError(errors).into())
}

/// The configuration a document deserializes to, and the top-level keys it has. The keys decide
/// which built-in sections have a location for their rules' errors: `limits` and `health_check`
/// have a plugin section even when the document has none.
pub(crate) struct ParsedConfiguration {
    config: Configuration,
    keys: BTreeSet<String>,
}

impl schemars::JsonSchema for ParsedConfiguration {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        Configuration::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        Configuration::json_schema(generator)
    }
}

/// Reads each top-level key into its typed setting, and each built-in plugin's section with its
/// plugin's factory, in one pass over the document. Every value is deserialized from the parser's
/// own deserializer, so an error inside a plugin's section is reported at its line.
impl<'de> serde::Deserialize<'de> for ParsedConfiguration {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ConfigurationVisitor)
    }
}

struct ConfigurationVisitor;

impl<'de> Visitor<'de> for ConfigurationVisitor {
    type Value = ParsedConfiguration;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("router configuration")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut config = Configuration::with_defaults();
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "reload" => config.reload = map.next_value()?,
                "health_check" => config.health_check = map.next_value()?,
                "sandbox" => config.sandbox = map.next_value()?,
                "homepage" => config.homepage = map.next_value()?,
                "server" => config.server = map.next_value()?,
                "supergraph" => config.supergraph = map.next_value()?,
                "cors" => config.cors = map.next_value()?,
                "tls" => config.tls = map.next_value()?,
                "apq" => config.apq = map.next_value()?,
                "persisted_queries" => config.persisted_queries = map.next_value()?,
                "limits" => config.limits = map.next_value()?,
                "plugins" => config.plugins = map.next_value()?,
                "batching" => config.batching = map.next_value()?,
                "experimental_type_conditioned_fetching" => {
                    config.experimental_type_conditioned_fetching = map.next_value()?
                }
                "experimental_hoist_orphan_errors" => {
                    config.experimental_hoist_orphan_errors = map.next_value()?
                }
                name => {
                    if !config.apollo_plugins.next_section(name, &mut map)? {
                        // Router's schema rejects unknown keys before deserialization.
                        return Err(serde::de::Error::custom(format!(
                            "no setting or built-in plugin reads the `{name}` section"
                        )));
                    }
                }
            }
            keys.insert(key);
        }
        config.insert_typed_plugin_sections();
        config.notify = Configuration::notify(&config.apollo_plugins)
            .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        let config = config
            .validate()
            .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        Ok(ParsedConfiguration { config, keys })
    }
}

/// Runs the validation rules of every plugin section the document has.
impl Validate for ParsedConfiguration {
    fn validate<'a>(&self, errors: ErrorCollector<'a>) {
        validate_plugin_sections(&self.config, errors, |name| self.keys.contains(name));
    }
}

impl apollo_configuration::Configuration for ParsedConfiguration {}

/// `--dev` settings, each replacing whatever the file sets at its path.
/// `include_subgraph_errors.all` is applied after parsing instead (see [`apply_dev_mode`]).
const DEV_MODE_SETTINGS: [(&[&str], bool); 6] = [
    (&["supergraph", "introspection"], true),
    (&["sandbox", "enabled"], true),
    (&["homepage", "enabled"], false),
    (&["expose_query_plan"], true),
    (
        &[
            "telemetry",
            "exporters",
            "tracing",
            "response_trace_id",
            "enabled",
        ],
        true,
    ),
    (&["connectors", "debug_extensions"], true),
];

const UPGRADE_GUIDE: &str =
    "https://www.apollographql.com/docs/graphos/routing/upgrade/from-router-v2";

/// Whether parsing first applies the current major version's migrations. Startup, reload and
/// `router config validate` do; tests can check a document as written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Migration {
    WithinMajor,
    /// Applies the same migrations without the summary "needs to be upgraded" error or the
    /// Rust-side migration logs, for callers that report migrations themselves. Each migration's
    /// own notices (`Action::Log`) still print, as they do at startup.
    WithinMajorQuietly,
    /// Parses the document as written, for configurations built in code.
    None,
}

/// Values the adapter reads from outside the document: expansion providers and injected
/// overrides. The adapter always adds Router's schema itself, because the retained document's
/// own schema accepts anything and would otherwise skip validation and type-directed coercion.
#[derive(Default)]
pub(crate) struct ExternalValues {
    variables: Vec<Box<dyn VariableProvider>>,
    injections: Vec<Injection>,
    dev_mode: bool,
}

impl ExternalValues {
    /// Appends an expansion provider, as [`ConfigParserBuilder::add_variables`] does.
    ///
    /// [`ConfigParserBuilder::add_variables`]: apollo_configuration::ConfigParserBuilder::add_variables
    pub(crate) fn add_variables(mut self, provider: impl VariableProvider + 'static) -> Self {
        self.variables.push(Box::new(provider));
        self
    }

    /// Appends injected values, as [`ConfigParserBuilder::inject`] does.
    ///
    /// [`ConfigParserBuilder::inject`]: apollo_configuration::ConfigParserBuilder::inject
    pub(crate) fn inject(mut self, injections: impl IntoIterator<Item = Injection>) -> Self {
        self.injections.extend(injections);
        self
    }

    /// Supplies the `--dev` settings as overrides named after the flag, so they go through the
    /// same parse as the file.
    pub(crate) fn dev_mode(mut self, dev_mode: bool) -> Self {
        self.dev_mode = dev_mode;
        self
    }

    /// Builds the parsers for the typed configuration and the retained document. Both share one
    /// snapshot of the providers, so they see the same expanded values.
    pub(crate) fn into_parser(mut self) -> Result<ConfigurationParser, ConfigError> {
        if self.dev_mode {
            self.injections.extend(
                DEV_MODE_SETTINGS
                    .iter()
                    .map(|(path, value)| Injection::cli_flag(path, Value::Bool(*value), "--dev")),
            );
        }
        let snapshot = self.snapshot();
        Ok(ConfigurationParser {
            config: parser_builder(&self.injections, &snapshot).build()?,
            document: parser_builder(&self.injections, &snapshot).build()?,
            dev_mode: self.dev_mode,
            snapshot,
        })
    }

    /// Moves the providers into a snapshot, or `None` when there are none.
    fn snapshot(&mut self) -> Option<Arc<ProviderSnapshot>> {
        (!self.variables.is_empty()).then(|| {
            Arc::new(ProviderSnapshot {
                providers: std::mem::take(&mut self.variables),
                resolved: Default::default(),
            })
        })
    }

    /// Expands `text` with these values but without Router's schema, for testing expansion on
    /// documents that are not router configuration.
    #[cfg(test)]
    pub(crate) fn expand_without_schema(mut self, text: &str) -> Result<Value, ConfigError> {
        let snapshot = self.snapshot();
        let mut builder = ConfigParser::<ExpandedDocument>::builder().inject(self.injections);
        if let Some(snapshot) = snapshot {
            builder = builder.add_variables(SharedSnapshot(snapshot));
        }
        let ExpandedDocument(document) = builder.build()?.parse_yaml(text)?;
        Ok(document)
    }
}

/// A parser builder with Router's schema, `injections` and, when there are providers, their
/// `snapshot`. Without providers, apollo-configuration leaves expansion syntax unchanged.
fn parser_builder<T: apollo_configuration::Configuration>(
    injections: &[Injection],
    snapshot: &Option<Arc<ProviderSnapshot>>,
) -> apollo_configuration::ConfigParserBuilder<T> {
    let builder = ConfigParser::builder()
        .schema(router_config_schema().clone())
        .inject(injections.to_vec());
    match snapshot {
        Some(snapshot) => builder.add_variables(SharedSnapshot(Arc::clone(snapshot))),
        None => builder,
    }
}

/// Parses Router YAML with a schema compiled once, fixed overrides, and fresh file expansions.
/// Own one parser for a sequence of configuration loads. Each load shares expansion values
/// across validation, the retained document, and migration fallback.
pub struct ConfigurationParser {
    config: ConfigParser<ParsedConfiguration>,
    document: ConfigParser<ExpandedDocument>,
    dev_mode: bool,
    snapshot: Option<Arc<ProviderSnapshot>>,
}

/// Consults each provider in order, as apollo-configuration does with separately added providers,
/// and remembers each result. Both parsing passes then see the same value, even if a file
/// or environment variable changes between them.
struct ProviderSnapshot {
    providers: Vec<Box<dyn VariableProvider>>,
    resolved: Mutex<HashMap<String, Result<String, LookupError>>>,
}

/// Lets both parsers read one [`ProviderSnapshot`].
struct SharedSnapshot(Arc<ProviderSnapshot>);

impl VariableProvider for SharedSnapshot {
    fn get(&self, reference: &str) -> Result<String, LookupError> {
        self.0.get(reference)
    }
}

impl ProviderSnapshot {
    fn get(&self, reference: &str) -> Result<String, LookupError> {
        self.resolved
            .lock()
            .entry(reference.to_string())
            .or_insert_with(|| {
                self.providers
                    .iter()
                    .map(|provider| provider.get(reference))
                    .find(|result| !matches!(result, Err(LookupError::UnrecognizedKind)))
                    .unwrap_or(Err(LookupError::UnrecognizedKind))
            })
            .clone()
    }
}

/// Retains document values without adding defaults from typed configuration serialization.
/// Router's schema governs its validation and expansion coercion.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
struct ExpandedDocument(Value);

impl apollo_configuration::Validate for ExpandedDocument {}
impl apollo_configuration::Configuration for ExpandedDocument {}

impl ConfigurationParser {
    /// Prepares configuration parsing with the process's environment and command-line inputs.
    /// Reuse this parser when loading a new configuration or reloading the same file.
    ///
    /// # Errors
    /// Returns invalid expansion-mode configuration or schema compilation errors.
    pub fn new() -> Result<Self, ConfigurationError> {
        Self::with_inputs(Expansion::default()?)
    }

    /// Prepares configuration parsing with the given environment and command-line inputs.
    pub(crate) fn with_inputs(expansion: Expansion) -> Result<Self, ConfigurationError> {
        Ok(ExternalValues::from(expansion).into_parser()?)
    }

    /// Parses configuration with within-major migrations and fresh file expansion values.
    /// A failed parse leaves the parser ready for the next load.
    ///
    /// # Errors
    /// Returns YAML, migration, expansion, override, schema, or typed configuration errors.
    pub fn parse(&mut self, text: &str) -> Result<Configuration, ConfigurationError> {
        self.parse_with_migration(text, Migration::WithinMajor)
    }

    /// Migrates the document, then parses the migrated copy, or the text as written when there is
    /// nothing to migrate or migration fails. Sandbox checks run last. Both parsing passes share
    /// one provider snapshot.
    pub(crate) fn parse_with_migration(
        &mut self,
        text: &str,
        migration: Migration,
    ) -> Result<Configuration, ConfigurationError> {
        // Clear on both success and failure (including unwinding), before another load can start.
        // In particular, a missing or rotated file must be read again on the next reload.
        scopeguard::defer! {
            if let Some(snapshot) = &self.snapshot {
                snapshot.resolved.lock().clear();
            }
        }
        // Migration serialization must not hide duplicate keys in the original document.
        super::yaml::check_duplicate_keys(text)?;
        let file: Value = if text.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_yaml::from_str(text).map_err(|error| {
                ConfigurationError::InvalidConfiguration {
                    message: "failed to parse yaml",
                    error: error.to_string(),
                }
            })?
        };
        let mut config = match migrate(&file, migration) {
            Ok(None) => parse_document(text, self).map_err(|error| report_error(error, &file)),
            Ok(Some(migrated)) => parse_document(&migrated, self).map_err(|error| {
                tracing::warn!(
                    "Configuration was upgraded automatically, then failed to load. Error locations refer to the upgraded configuration, not to your file."
                );
                report_error(error, &file)
            }),
            Err(error) => {
                tracing::warn!(
                    "Configuration could not be upgraded automatically, so it is loaded as written: {error}. If you are upgrading from Router 2.x, please refer to the upgrade guide: {UPGRADE_GUIDE}"
                );
                parse_document(text, self).map_err(|error| report_error(error, &file))
            }
        }?;
        if self.dev_mode {
            apply_dev_mode(&mut config);
        }
        // `--dev` sets these settings, so they are checked only once it has been applied.
        config.validate_sandbox_settings()?;
        config.raw_yaml = Some(Arc::from(text));
        Ok(config)
    }
}

/// The document with `migration` applied, serialized for parsing, or `None` when migration
/// changes nothing. An error means migration itself failed, and the caller loads the text as
/// written instead.
fn migrate(file: &Value, migration: Migration) -> Result<Option<String>, ConfigurationError> {
    let migrated = match migration {
        Migration::WithinMajor => upgrade_configuration(file, true, UpgradeMode::current_minor())?,
        Migration::WithinMajorQuietly => {
            upgrade_configuration(file, false, UpgradeMode::current_minor())?
        }
        Migration::None => return Ok(None),
    };
    if migrated == *file {
        return Ok(None);
    }
    serde_yaml::to_string(&migrated).map(Some).map_err(|error| {
        ConfigurationError::MigrationFailure {
            error: error.to_string(),
        }
    })
}

/// Sets `include_subgraph_errors.all: true` for `--dev`, replacing whatever the file set there,
/// as earlier releases did. An override cannot do this: apollo-configuration refuses to replace
/// an object with settings, and `all` can be an allow or deny list.
fn apply_dev_mode(config: &mut Configuration) {
    const NAME: &str = "include_subgraph_errors";
    let factory = super::plugin_configs::find_factory(&format!("apollo.{NAME}"))
        .expect("the include_subgraph_errors plugin is registered");
    let current = config
        .apollo_plugins
        .get(&format!("apollo.{NAME}"))
        .map(|parsed| &parsed.config);
    let config_with_errors =
        crate::plugins::include_subgraph_errors::with_all_errors_included(current);
    config.apollo_plugins.insert(
        NAME,
        super::ParsedPlugin {
            factory,
            config: config_with_errors,
        },
    );
    let document = config
        .validated_yaml
        .get_or_insert_with(|| Value::Object(Default::default()));
    let section = &mut document[NAME];
    if !section.is_object() {
        *section = json!({});
    }
    section["all"] = Value::Bool(true);
}

/// Parses `text` as written, with no expansion, overrides, migration or `--dev`, for
/// configurations built in code. Parses share one parser, so Router's schema is compiled once.
pub(crate) fn parse_as_written(text: &str) -> Result<Configuration, ConfigurationError> {
    static PARSER: OnceLock<Mutex<ConfigurationParser>> = OnceLock::new();
    PARSER
        .get_or_init(|| {
            Mutex::new(
                ExternalValues::default()
                    .into_parser()
                    .expect("Router's schema compiles"),
            )
        })
        .lock()
        .parse_with_migration(text, Migration::None)
}

/// Builds independent parsers for tests supplying their own inputs.
#[cfg(test)]
pub(crate) fn parse_configuration(
    text: &str,
    external: impl Into<ExternalValues>,
    migration: Migration,
) -> Result<Configuration, ConfigurationError> {
    external
        .into()
        .into_parser()?
        .parse_with_migration(text, migration)
}

/// Suggests `router config upgrade` for a configuration that fails validation, when that upgrade
/// would change the operator's `file`. The upgrade is run here without logging any of its notices,
/// and only on failure, so a configuration that loads pays nothing for it. A failing upgrade gives
/// no hint.
fn report_error(error: ConfigError, file: &Value) -> ConfigurationError {
    if matches!(error, ConfigError::ValidationError(_)) && upgrade_would_change(file) {
        tracing::warn!(
            "Configuration had errors. It may be possible to update your configuration automatically. Execute 'router config upgrade --help' for more details. If you are upgrading from Router 2.x, please refer to the upgrade guide: {UPGRADE_GUIDE}"
        );
    }
    ConfigurationError::from(error)
}

/// Whether the upgrade `router config upgrade` performs changes `file`.
fn upgrade_would_change(file: &Value) -> bool {
    upgrade_configuration_silently(file, UpgradeMode::Major).is_ok_and(|upgraded| upgraded != *file)
}

/// Parses the typed configuration, then the expanded document, with the same options. The retained
/// document shows `plugins: null` as `{}`, which means the same.
///
/// apollo-configuration returns the typed value only, not the expanded document it validated, so
/// the document is read by a second parser that shares the first's provider snapshot.
fn parse_document(text: &str, parser: &ConfigurationParser) -> Result<Configuration, ConfigError> {
    let ParsedConfiguration { mut config, .. } = parser.config.parse_yaml(text)?;
    let ExpandedDocument(mut document) = parser.document.parse_yaml(text)?;
    if let Some(plugins) = document
        .get_mut("plugins")
        .filter(|plugins| plugins.is_null())
    {
        *plugins = json!({});
    }
    config.validated_yaml = Some(document);
    Ok(config)
}

/// Runs `run` inside a span unique to this call, then checks the lines logged in that span. Tests
/// share one log buffer, so a check that a line is absent must look only at its own lines.
#[cfg(test)]
pub(crate) fn assert_logs<T>(
    run: impl FnOnce() -> T,
    check: impl Fn(&[&str]) -> Result<(), String>,
) -> T {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use crate::test_harness::tracing_test;

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let _guard = tracing_test::dispatcher_guard();
    let result = tracing::info_span!("assert_logs", call).in_scope(run);
    tracing_test::logs_with_scope_assert(&format!("assert_logs{{call={call}}}"), check).unwrap();
    result
}

/// Checks that the migrated copy's errors were reported, with a warning that says so, and that
/// the file as written was not loaded instead.
#[cfg(test)]
pub(crate) fn migrated_copy_warning(lines: &[&str]) -> Result<(), String> {
    if let Some(line) = lines
        .iter()
        .find(|line| line.contains("could not be upgraded automatically"))
    {
        return Err(format!("the file must not be loaded instead: {line}"));
    }
    lines
        .iter()
        .any(|line| line.contains("refer to the upgraded configuration"))
        .then_some(())
        .ok_or_else(|| "the warning must say which document the errors refer to".into())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use apollo_configuration::expansion::MapVariables;
    use apollo_configuration::expansion::argument_for_kind;
    use serde_json::json;

    use super::*;

    fn parse(text: &str) -> Result<Configuration, ConfigurationError> {
        parse_configuration(text, ExternalValues::default(), Migration::WithinMajor)
    }

    fn env(name: &str, value: &str) -> ExternalValues {
        ExternalValues::default().add_variables(MapVariables(HashMap::from([(
            name.to_string(),
            value.to_string(),
        )])))
    }

    #[test]
    fn rejected_input_renders_as_a_miette_diagnostic() {
        let text = include_str!("testdata/compat/unknown_top_level_key.yaml");

        let error = parse(text).expect_err("the fixture sets a key the schema does not declare");

        let ConfigurationError::ApolloConfiguration(rendered) = &error else {
            panic!("expected ApolloConfiguration, got: {error:?}");
        };
        assert!(
            rendered.contains("Additional properties are not allowed"),
            "rendered diagnostic should name the schema violation: {rendered}"
        );
        assert!(
            rendered.contains("this_key_does_not_exist_anywhere"),
            "rendered diagnostic should quote the offending key: {rendered}"
        );
    }

    #[test]
    fn diagnostics_redact_typed_secrets_reached_through_draft_7_references() {
        let secret = "redis-password-must-not-leak"; // gitleaks:allow
        let text = format!(
            "apq:\n  router:\n    cache:\n      redis:\n        urls: [redis://localhost]\n        password: {secret}\n        unexpected: true\n"
        );

        let error = parse(&text).expect_err("the Redis configuration contains an unknown field");
        let rendered = error.to_string();

        assert!(
            rendered.contains("[REDACTED]"),
            "the diagnostic should show that the password was redacted: {rendered}"
        );
        assert!(
            !rendered.contains(secret),
            "the diagnostic must not contain the typed secret: {rendered}"
        );
    }

    #[test]
    fn retained_document_preserves_secret_values_without_serializing_the_typed_configuration() {
        let text = "apq:\n  router:\n    cache:\n      redis:\n        urls: [redis://localhost]\n        password: ${env.ADAPTER_PASSWORD}\n";
        let secret = "adapter-test-password"; // gitleaks:allow
        let config = parse_configuration(text, env("ADAPTER_PASSWORD", secret), Migration::None)
            .expect("valid Redis settings");

        let redis = config.apq.router.cache.redis.as_ref().unwrap();
        assert_eq!(redis.password.as_ref().unwrap().unredact(), secret);
        let document = config.validated_yaml.as_ref().unwrap();
        assert_eq!(
            document["apq"]["router"]["cache"]["redis"]["password"],
            secret
        );
        assert_eq!(config.raw_yaml.as_deref(), Some(text));
    }

    #[test]
    fn malformed_yaml_returns_a_parse_error() {
        let error = parse("supergraph: [").expect_err("the sequence is unterminated");

        assert!(matches!(
            error,
            ConfigurationError::InvalidConfiguration {
                message: "could not parse yaml",
                ..
            }
        ));
        assert!(
            error
                .to_string()
                .contains("expected node content at line 2 column 1"),
            "{error}"
        );
    }

    #[test]
    fn unsupported_expansion_returns_the_reference() {
        let error = parse_configuration(
            "supergraph:\n  listen: ${unsupported.ADDRESS}\n",
            ExternalValues::default().add_variables(MapVariables(HashMap::new())),
            Migration::None,
        )
        .expect_err("the expansion kind is unsupported");

        assert!(matches!(error, ConfigurationError::ApolloConfiguration(_)));
        assert!(error.to_string().contains("unsupported.ADDRESS"), "{error}");
    }

    #[test]
    fn blank_documents_are_the_default_configuration() {
        for text in ["", "  \n"] {
            let config = parse(text).expect("a blank document is valid");
            assert_eq!(config.validated_yaml, Some(json!({})), "{text:?}");
            assert_eq!(config.raw_yaml.as_deref(), Some(text));
            assert_eq!(
                config.supergraph.listen.to_string(),
                "http://127.0.0.1:4000"
            );
        }
    }

    #[test]
    fn null_plugins_mean_no_user_plugins() {
        let config = parse("plugins: null\n").expect("null plugin config is accepted");

        assert_eq!(config.plugins.iter().count(), 0);
        assert_eq!(config.validated_yaml, Some(json!({ "plugins": {} })));
    }

    /// Empty `plugins:` is parsed as written, so an error elsewhere in the file is located in the
    /// file, whether or not startup migration runs.
    #[test]
    fn errors_beside_null_plugins_refer_to_the_file_as_written() {
        let text = "# every plugin commented out\nplugins:\nsupergraph:\n  listen: 12\n";
        for migration in [Migration::WithinMajor, Migration::None] {
            let error = parse_configuration(text, ExternalValues::default(), migration)
                .expect_err("the listen address is invalid")
                .to_string();

            // The comment makes `listen` line 4 of the file; a re-serialized copy has no comment.
            assert!(error.contains("[4:"), "{error}");
        }
    }

    #[test]
    fn expansion_coerces_through_routers_schema_in_both_passes() {
        let config = parse_configuration(
            "supergraph:\n  introspection: ${env.FLAG}\n",
            env("FLAG", "true"),
            Migration::None,
        )
        .expect("the reference resolves to a boolean");

        assert!(config.supergraph.introspection);
        assert_eq!(
            config.validated_yaml.as_ref().unwrap()["supergraph"]["introspection"],
            json!(true),
            "the retained document must hold the coerced boolean, not the string \"true\""
        );
    }

    /// Returns a different password on every read, like a secret file rotated between reads.
    struct RotatingPassword(Arc<AtomicUsize>);

    impl VariableProvider for RotatingPassword {
        fn get(&self, reference: &str) -> Result<String, LookupError> {
            argument_for_kind(reference, "env")?;
            let read = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(format!("password-read-{read}")) // gitleaks:allow
        }
    }

    #[test]
    fn reused_parser_keeps_each_load_consistent() {
        let text = indoc::indoc!(
            "
            apq:
              router:
                cache:
                  redis:
                    urls: [redis://localhost]
                    password: ${env.PW}
        "
        );
        let reads = Arc::new(AtomicUsize::new(0));
        let mut parser = ExternalValues::default()
            .add_variables(RotatingPassword(reads.clone()))
            .into_parser()
            .unwrap();

        for i in 0..3 {
            let config = parser.parse(text).expect("valid Redis config");
            let password = format!("password-read-{i}");
            assert_eq!(
                config
                    .apq
                    .router
                    .cache
                    .redis
                    .as_ref()
                    .unwrap()
                    .password
                    .as_ref()
                    .unwrap()
                    .unredact(),
                &password
            );
            assert_eq!(
                config.validated_yaml.as_ref().unwrap()["apq"]["router"]["cache"]["redis"]["password"],
                password
            );
        }
        assert_eq!(reads.load(Ordering::SeqCst), 3, "one lookup per load");
    }

    #[test]
    fn reused_parser_reloads_files_after_success_and_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("password.txt");
        let reference = serde_json::to_string(&format!("${{file.{}}}", path.display())).unwrap();
        let text = format!(
            indoc::indoc!(
                "
            apq:
              router:
                cache:
                  redis:
                    urls: [redis://localhost]
                    password: {reference}
        "
            ),
            reference = reference
        );
        let mut parser = ConfigurationParser::new().unwrap();
        assert!(parser.parse(&text).is_err(), "file is initially missing");
        for password in ["first-value", "rotated-value"] {
            std::fs::write(&path, password).unwrap();
            let config = parser.parse(&text).unwrap();
            assert_eq!(
                config
                    .apq
                    .router
                    .cache
                    .redis
                    .as_ref()
                    .unwrap()
                    .password
                    .as_ref()
                    .unwrap()
                    .unredact(),
                password
            );
            assert_eq!(
                config.validated_yaml.as_ref().unwrap()["apq"]["router"]["cache"]["redis"]["password"],
                password
            );
        }
        std::fs::remove_file(&path).unwrap();
        assert!(
            parser.parse(&text).is_err(),
            "removed file must not stay cached"
        );
        std::fs::write(&path, "restored-value").unwrap();
        assert!(parser.parse(&text).is_ok());
    }

    /// Redis settings whose non-secret `namespace` anchors the value aliased into `password`,
    /// plus an unknown key so that the diagnostic shows the anchor.
    const ANCHORED_SECRET: &str = "apq:\n  router:\n    cache:\n      redis:\n        urls: [redis://localhost]\n        namespace: &pw anchored-secret-value\n        unexpected: true\n        password: *pw\n"; // gitleaks:allow

    #[test]
    fn diagnostics_redact_an_anchor_aliased_into_a_secret_field() {
        let error =
            parse(ANCHORED_SECRET).expect_err("the Redis configuration contains an unknown field");
        let rendered = error.to_string();

        assert!(
            rendered.contains("namespace: &pw [REDACTED]"),
            "the diagnostic should quote the original text with the anchor redacted: {rendered}"
        );
        assert!(
            !rendered.contains("anchored-secret-value"),
            "the diagnostic must not contain the aliased secret: {rendered}"
        );
    }

    /// Pins a known limitation: a migrated copy has no YAML aliases, so when it fails, the anchor
    /// aliased into `password` is quoted unredacted at the non-secret field that anchors it. The
    /// secret field itself stays redacted. Migrating without losing anchors removes this.
    #[test]
    fn migrated_documents_lose_anchor_redaction() {
        let text = format!("cors:\n  origins:\n    - https://example.com\n{ANCHORED_SECRET}");

        let rendered = parse(&text)
            .expect_err("the Redis configuration contains an unknown field")
            .to_string();

        assert!(rendered.contains("password: [REDACTED]"), "{rendered}");
        assert!(
            rendered.contains("namespace: anchored-secret-value"),
            "expected the known limitation; update this test if anchors survive migration: {rendered}"
        );
    }

    #[test]
    fn migration_reports_what_it_changed() {
        assert_logs(
            || {
                parse(include_str!(
                    "testdata/compat/needs_minor_migration_cors_origins.yaml"
                ))
            },
            |lines| {
                lines
                    .iter()
                    .any(|line| line.contains("needs to be upgraded"))
                    .then_some(())
                    .ok_or_else(|| {
                        "the adapter must report applied migrations like the production loader"
                            .to_string()
                    })
            },
        )
        .expect("the adapter migrates legacy CORS settings");
    }

    /// `router config validate` reports migrations itself, so its parse omits the summary error.
    /// Each migration's own notice still prints, as it does at startup.
    #[test]
    fn quiet_migration_omits_the_upgrade_required_error() {
        assert_logs(
            || {
                parse_configuration(
                    include_str!("testdata/compat/needs_minor_migration_cors_origins.yaml"),
                    ExternalValues::default(),
                    Migration::WithinMajorQuietly,
                )
            },
            |lines| {
                if let Some(line) = lines
                    .iter()
                    .find(|line| line.contains("needs to be upgraded"))
                {
                    return Err(format!("unexpected upgrade-required error: {line}"));
                }
                lines
                    .iter()
                    .any(|line| line.contains("CORS configuration has been migrated"))
                    .then_some(())
                    .ok_or_else(|| "the CORS migration's own notice must still print".to_string())
            },
        )
        .expect("the adapter migrates legacy CORS settings");
    }

    #[test]
    fn diagnostics_refer_to_the_original_text_when_migration_changes_nothing() {
        let text = "# one\n# two\n\n# three\n# four\n\nsupergraph:\n  # listen comment\n  listen: 127.0.0.1:0\n\nthis_key_does_not_exist_anywhere: true\n";

        let error = parse(text).expect_err("the document sets a key the schema does not declare");
        let rendered = error.to_string();

        assert!(
            rendered.contains("[11:1]"),
            "the diagnostic should point at the offending line of the original text: {rendered}"
        );
    }

    /// A file that fails validation and that `router config upgrade` would change gets a hint to
    /// run it.
    #[test]
    fn validation_errors_suggest_router_config_upgrade_when_it_would_change_the_file() {
        let text = "cors:\n  origins:\n    - https://example.com\nthis_key_does_not_exist_anywhere: true\n";

        assert_logs(
            || parse(text),
            |lines| {
                // Startup's migration notice, once: checking for the hint logs no notices.
                let notices = lines
                    .iter()
                    .filter(|line| line.contains("CORS configuration has been migrated"))
                    .count();
                if notices != 1 {
                    return Err(format!("expected one CORS migration notice, got {notices}"));
                }
                lines
                    .iter()
                    .any(|line| {
                        line.contains("router config upgrade") && line.contains("from-router-v2")
                    })
                    .then_some(())
                    .ok_or_else(|| "expected a hint to run `router config upgrade`".to_string())
            },
        )
        .expect_err("the key is unknown");
    }

    /// Checking whether `router config upgrade` would change a file logs nothing.
    #[test]
    fn checking_for_the_upgrade_hint_logs_nothing() {
        let file = json!({ "cors": { "origins": ["https://example.com"] } });

        let changed = assert_logs(
            || upgrade_would_change(&file),
            |lines| match lines.first() {
                Some(line) => Err(format!("unexpected log line: {line}")),
                None => Ok(()),
            },
        );

        assert!(changed, "the CORS migration changes the file");
    }

    /// A file that fails validation but that `router config upgrade` would not change gets no
    /// hint.
    #[test]
    fn validation_errors_do_not_suggest_router_config_upgrade_when_it_changes_nothing() {
        assert_logs(
            || parse("this_key_does_not_exist_anywhere: true\n"),
            no_upgrade_hint,
        )
        .expect_err("the key is unknown");
    }

    /// Parsing stops at the first plugin section that fails to deserialize, and reports it at the
    /// value that failed, inside the plugin's section.
    #[test]
    fn plugin_deserialization_errors_point_at_the_value() {
        let text = "# operator comment\ntraffic_shaping:\n  router:\n    timeout: not-a-duration\nsubscription:\n  deduplication:\n    enabled: true\n";

        let error = parse_configuration(text, ExternalValues::default(), Migration::None)
            .expect_err("both plugins' config is invalid")
            .to_string();

        assert!(error.contains("apollo.traffic_shaping"), "{error}");
        assert!(error.contains("[4:14]"), "{error}");
        assert!(error.contains("timeout: not-a-duration"), "{error}");
    }

    /// Test plugins whose custom rules reject a `name` that the schema accepts: a user plugin,
    /// `test.validated`, and a built-in one, `test_validated`, whose rule is on a nested setting.
    mod validated_plugin {
        use apollo_configuration::ErrorCollector;
        use apollo_configuration::configuration;
        use tower::BoxError;

        use crate::plugin::Plugin;
        use crate::plugin::PluginInit;

        /// A nested setting with a custom validation rule.
        #[configuration(validate = reject_reserved_inner_name)]
        pub(super) struct NestedName {
            /// Any name except `reserved`.
            name: String,
        }

        fn reject_reserved_inner_name(config: &NestedName, mut errors: ErrorCollector<'_>) {
            if config.name == "reserved" {
                errors
                    .nest("name")
                    .report_simple("the nested name `reserved` is not allowed");
            }
        }

        /// A test built-in plugin with a validated nested setting.
        #[configuration]
        pub(super) struct BuiltInValidatedConfig {
            /// A setting with its own validation rule.
            inner: NestedName,
        }

        struct BuiltInValidatedPlugin;

        #[async_trait::async_trait]
        impl Plugin for BuiltInValidatedPlugin {
            type Config = BuiltInValidatedConfig;

            async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
                Ok(Self)
            }
        }

        register_plugin!("apollo", "test_validated", BuiltInValidatedPlugin);

        /// A test plugin with a custom validation rule.
        #[configuration(validate = reject_reserved_name)]
        pub(super) struct ValidatedConfig {
            /// Any name except `reserved`.
            name: String,
            /// A nested section with its own rule.
            nested: Option<ValidatedNestedConfig>,
        }

        /// A nested section whose rule runs because its parent's field doesn't skip validation.
        #[configuration(validate = reject_zero_limit)]
        pub(super) struct ValidatedNestedConfig {
            /// At least 1.
            #[config(required)]
            limit: u32,
        }

        fn reject_zero_limit(config: &ValidatedNestedConfig, mut errors: ErrorCollector<'_>) {
            if config.limit == 0 {
                errors
                    .nest("limit")
                    .report_simple("limit must be at least 1");
            }
        }

        fn reject_reserved_name(config: &ValidatedConfig, mut errors: ErrorCollector<'_>) {
            if config.name == "reserved" {
                errors
                    .nest("name")
                    .report_simple("the name `reserved` is not allowed");
            }
        }

        struct ValidatedPlugin;

        #[async_trait::async_trait]
        impl Plugin for ValidatedPlugin {
            type Config = ValidatedConfig;

            async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
                Ok(Self)
            }
        }

        register_plugin!("test", "validated", ValidatedPlugin);
    }

    /// A plugin's own validation rules run while the configuration is parsed, and their errors
    /// quote the plugin's section of the file.
    #[test]
    fn plugin_validation_rules_reject_schema_valid_values_at_their_section() {
        let text = "# operator comment\nplugins:\n  test.validated:\n    name: reserved\n";

        let error = parse_configuration(text, ExternalValues::default(), Migration::None)
            .expect_err("the plugin's rule rejects the name")
            .to_string();

        assert!(
            error.contains("the name `reserved` is not allowed"),
            "{error}"
        );
        assert!(error.contains("[4:11]"), "{error}");
        assert!(error.contains("name: reserved"), "{error}");
    }

    /// A nested type's rule runs too, and its error quotes the nested key.
    #[test]
    fn nested_validation_rules_reject_schema_valid_values_at_their_key() {
        let text = "plugins:\n  test.validated:\n    nested:\n      limit: 0\n";

        let error = parse_configuration(text, ExternalValues::default(), Migration::None)
            .expect_err("the nested rule rejects the limit")
            .to_string();

        assert!(error.contains("limit must be at least 1"), "{error}");
        assert!(error.contains("[4:14]"), "{error}");
    }

    #[test]
    fn plugin_validation_rules_accept_other_values() {
        let config = parse_configuration(
            "plugins:\n  test.validated:\n    name: allowed\n",
            ExternalValues::default(),
            Migration::None,
        )
        .expect("the plugin's rule accepts the name");

        assert!(config.plugins.get("test.validated").is_some());
    }

    /// A built-in plugin's rule on a nested setting reports at that setting's line and column.
    #[test]
    fn built_in_plugin_rules_report_at_the_nested_setting() {
        let text = "# operator comment\ntest_validated:\n  inner:\n    name: reserved\n";

        let error = parse_configuration(text, ExternalValues::default(), Migration::None)
            .expect_err("the built-in plugin's rule rejects the name")
            .to_string();

        assert!(
            error.contains("the nested name `reserved` is not allowed"),
            "{error}"
        );
        assert!(error.contains("[4:11]"), "{error}");
        assert!(error.contains("name: reserved"), "{error}");
    }

    /// Every rule failure is reported, across built-in and user plugins.
    #[test]
    fn every_plugin_failing_validation_is_reported() {
        let text = "test_validated:\n  inner:\n    name: reserved\nplugins:\n  test.validated:\n    name: reserved\n";

        let error = parse_configuration(text, ExternalValues::default(), Migration::None)
            .expect_err("both plugins' rules reject their names")
            .to_string();

        assert!(
            error.contains("the nested name `reserved` is not allowed"),
            "{error}"
        );
        assert!(error.contains("[3:11]"), "{error}");
        assert!(
            error.contains("the name `reserved` is not allowed"),
            "{error}"
        );
        assert!(error.contains("[6:11]"), "{error}");
    }

    /// An override reaches plugin rules through the same parse, and the rule's error names the
    /// override rather than a line of the file. `--dev` supplies its settings this way. The
    /// message quotes the overridden value, so apollo-configuration masks it.
    #[test]
    fn plugin_rules_see_overridden_values() {
        let external = ExternalValues::default().inject([Injection::cli_flag(
            &["plugins", "test.validated", "name"],
            json!("reserved"),
            "--name",
        )]);

        let error = parse_configuration(
            "plugins:\n  test.validated:\n    name: allowed\n",
            external,
            Migration::None,
        )
        .expect_err("the overridden name is rejected")
        .to_string();

        assert!(error.contains("value failed custom validation"), "{error}");
        assert!(error.contains("--name"), "{error}");
    }

    /// Configurations built with serde or the test builders go through the same rules.
    #[test]
    fn serde_and_builder_configurations_run_plugin_rules() {
        let error = serde_yaml::from_str::<Configuration>(
            "plugins:\n  test.validated:\n    name: reserved\n",
        )
        .expect_err("the plugin's rule rejects the name")
        .to_string();
        assert!(
            error.contains("the name `reserved` is not allowed"),
            "{error}"
        );

        let error = Configuration::builder()
            .apollo_plugin("test_validated", json!({ "inner": { "name": "reserved" } }))
            .build()
            .expect_err("the built-in plugin's rule rejects the name")
            .to_string();
        assert!(
            error.contains("the nested name `reserved` is not allowed"),
            "{error}"
        );
    }

    /// A migrated copy that fails only its plugins' rules is not replaced by the file, which
    /// would fail on the settings migration fixed and hide the rule's error.
    #[test]
    fn migrated_documents_failing_plugin_rules_report_the_rule() {
        let text = "cors:\n  origins:\n    - https://example.com\nplugins:\n  test.validated:\n    name: reserved\n";

        let error = assert_logs(|| parse(text), migrated_copy_warning)
            .expect_err("the plugin's rule rejects the name")
            .to_string();

        assert!(
            error.contains("the name `reserved` is not allowed"),
            "{error}"
        );
        assert!(!error.contains("origins"), "{error}");
    }

    /// A file that fails only a plugin's rule, which `router config upgrade` would not change,
    /// gets no hint.
    #[test]
    fn plugin_rule_failures_do_not_suggest_router_config_upgrade() {
        assert_logs(
            || parse("plugins:\n  test.validated:\n    name: reserved\n"),
            no_upgrade_hint,
        )
        .expect_err("the plugin's rule rejects the name");
    }

    /// Once migration succeeds, the migrated copy is the document that is loaded. A schema error in
    /// it is reported from that copy, with a warning that locations refer to it, and the file as
    /// written is not parsed again.
    #[test]
    fn schema_errors_after_migration_are_reported_from_the_migrated_copy() {
        let text = "# operator comment\ncors:\n  origins:\n    - \"https://example.com\"\nthis_key_does_not_exist_anywhere: true\n";

        let error = assert_logs(|| parse(text), migrated_copy_warning)
            .expect_err("the unknown key is invalid in the migrated copy")
            .to_string();

        assert!(
            error.contains("this_key_does_not_exist_anywhere"),
            "{error}"
        );
        // The key is on line 6 of the migrated copy, which moves `origins` under `policies`, and
        // on line 5 of the file.
        assert!(error.contains("[6:1]"), "{error}");
        assert!(!error.contains("[5:1]"), "{error}");
    }

    /// Expansion errors after a successful migration are reported from the migrated copy too.
    #[test]
    fn expansion_errors_after_migration_are_reported_from_the_migrated_copy() {
        // Migration 2045 moves the unresolvable reference under `deduplication.all`.
        let text =
            "# operator comment\nsubscription:\n  deduplication:\n    enabled: ${env.MISSING}\n";

        let error = assert_logs(
            || parse_configuration(text, env("PRESENT", "1"), Migration::WithinMajor),
            migrated_copy_warning,
        )
        .expect_err("the reference cannot be resolved")
        .to_string();

        assert!(error.contains("expansion value not present"), "{error}");
        assert!(error.contains("all:"), "{error}");
        assert!(!error.contains("# operator comment"), "{error}");
    }

    /// When migration itself fails, the file is loaded as written, so its diagnostics quote the
    /// file. Here the legacy `origins` cannot be moved into a `policies` that is not a list. The
    /// `router config upgrade` dry run fails the same way, so there is no hint to run it.
    #[test]
    fn failed_migrations_load_the_file_as_written() {
        let text =
            "# operator comment\ncors:\n  origins:\n    - \"https://example.com\"\n  policies: 3\n";

        let error = assert_logs(
            || parse(text),
            |lines| {
                no_upgrade_hint(lines)?;
                lines
                    .iter()
                    .any(|line| line.contains("could not be upgraded automatically"))
                    .then_some(())
                    .ok_or_else(|| "the fallback must warn that the upgrade failed".to_string())
            },
        )
        .expect_err("the file as written is invalid")
        .to_string();

        // The file's own lines: the comment makes `origins` line 3 and `policies` line 5.
        assert!(error.contains("'origins' was unexpected"), "{error}");
        assert!(error.contains("[3:3]"), "{error}");
        assert!(error.contains("[5:13]"), "{error}");
    }

    fn no_upgrade_hint(lines: &[&str]) -> Result<(), String> {
        match lines
            .iter()
            .find(|line| line.contains("router config upgrade"))
        {
            Some(line) => Err(format!("unexpected upgrade hint: {line}")),
            None => Ok(()),
        }
    }
}
