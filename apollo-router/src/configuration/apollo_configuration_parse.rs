//! Parses router configuration with `apollo-configuration`.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_configuration::ConfigError;
use apollo_configuration::ConfigParser;
use apollo_configuration::ErrorCollector;
use apollo_configuration::expansion::LookupError;
use apollo_configuration::expansion::VariableProvider;
use apollo_configuration::provenance::Injection;
use parking_lot::Mutex;
use serde_json::Value;
use serde_json::json;

use super::Configuration;
use super::ConfigurationError;
use super::expansion::Expansion;
use super::schema::router_config_schema;
use super::upgrade::UpgradeMode;
use super::upgrade::upgrade_configuration;

/// Reports every plugin whose config could not be deserialized, at its section of the document.
impl apollo_configuration::Validate for Configuration {
    fn validate<'a>(&self, mut errors: ErrorCollector<'a>) {
        for error in self.plugin_configs.errors() {
            report_at(
                errors.inner(),
                &error.section,
                error.to_configuration_error().to_string(),
            );
        }
    }
}

fn report_at(mut errors: ErrorCollector<'_>, path: &[String], message: String) {
    match path.split_first() {
        Some((segment, rest)) => report_at(errors.nest(segment.as_str()), rest, message),
        None => errors.report_simple(message),
    }
}

impl apollo_configuration::Configuration for Configuration {}

const UPGRADE_GUIDE: &str =
    "https://www.apollographql.com/docs/graphos/routing/upgrade/from-router-v2";

/// Whether parsing first applies the current major version's migrations. Startup, reload and
/// `router config validate` do; tests can check a document as written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Migration {
    WithinMajor,
    /// Applies the same migrations without logging them, for callers that report them
    /// themselves.
    WithinMajorQuietly,
    #[cfg(test)]
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

    /// Applies the `--dev` config once the document has been migrated and parsed.
    pub(crate) fn dev_mode(mut self, dev_mode: bool) -> Self {
        self.dev_mode = dev_mode;
        self
    }

    /// Builds the parsers for the typed configuration and the retained document. Both share one
    /// snapshot of the providers, so they see the same expanded values.
    pub(crate) fn into_parser(mut self) -> Result<ConfigurationParser, ConfigError> {
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
    config: ConfigParser<Configuration>,
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

    /// Parses the original text, falling back to it if a migrated copy fails. Dev config and
    /// sandbox checks run last. Both parsing passes and fallback share one provider snapshot.
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
        let migrated = match migration {
            Migration::WithinMajor => {
                upgrade_configuration(&file, true, UpgradeMode::current_minor())?
            }
            Migration::WithinMajorQuietly => {
                upgrade_configuration(&file, false, UpgradeMode::current_minor())?
            }
            #[cfg(test)]
            Migration::None => file.clone(),
        };
        let mut config = if migrated == file {
            parse_document(text, self).map_err(report_error)
        } else {
            let serialized = serde_yaml::to_string(&migrated).map_err(|error| {
                ConfigurationError::MigrationFailure {
                    error: error.to_string(),
                }
            })?;
            // Diagnostics for the serialized copy would point at lines the operator never wrote, so
            // any error in it falls back to the supplied text. The fallback still validates that text
            // in full, so it never accepts an invalid document.
            match parse_document(&serialized, self) {
                Ok(config) => Ok(config),
                Err(_) => {
                    tracing::warn!(
                        "Configuration could not be upgraded automatically as it had errors. If you are upgrading from Router 2.x, please refer to the upgrade guide: {UPGRADE_GUIDE}"
                    );
                    parse_document(text, self).map_err(report_error)
                }
            }
        }?;
        if self.dev_mode {
            config.apply_dev_mode();
        }
        // `--dev` sets these settings, so they are checked only once it has been applied.
        config.validate_sandbox_settings()?;
        config.raw_yaml = Some(Arc::from(text));
        Ok(config)
    }
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

/// Suggests `router config upgrade` for a configuration that fails validation.
fn report_error(error: ConfigError) -> ConfigurationError {
    if matches!(error, ConfigError::ValidationError(_)) {
        tracing::warn!(
            "Configuration had errors. It may be possible to update your configuration automatically. Execute 'router config upgrade --help' for more details. If you are upgrading from Router 2.x, please refer to the upgrade guide: {UPGRADE_GUIDE}"
        );
    }
    ConfigurationError::from(error)
}

/// Parses the typed configuration, then the expanded document, with the same options. The retained
/// document shows `plugins: null` as `{}`, which means the same.
fn parse_document(text: &str, parser: &ConfigurationParser) -> Result<Configuration, ConfigError> {
    let mut config = parser.config.parse_yaml(text)?;
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use apollo_configuration::expansion::MapVariables;
    use apollo_configuration::expansion::argument_for_kind;
    use serde_json::json;

    use super::*;
    use crate::test_harness::tracing_test;

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

        assert!(config.plugins.plugins.unwrap_or_default().is_empty());
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

    /// A migrated copy has no YAML aliases, so its diagnostics could not redact an anchor aliased
    /// into a secret field. When the copy fails, only the file's errors are reported, with the
    /// anchor redacted.
    #[test]
    fn migrated_documents_that_fall_back_redact_anchor_sources() {
        let text = format!("cors:\n  origins:\n    - https://example.com\n{ANCHORED_SECRET}");

        let error = parse(&text).expect_err("the Redis configuration contains an unknown field");
        let rendered = error.to_string();

        assert!(
            rendered.contains("namespace: &pw [REDACTED]"),
            "the fallback should quote the file with the anchor redacted: {rendered}"
        );
        assert!(
            !rendered.contains("anchored-secret-value"),
            "the diagnostic must not contain the aliased secret: {rendered}"
        );
    }

    #[test]
    fn migration_reports_what_it_changed() {
        let _guard = tracing_test::dispatcher_guard();

        parse(include_str!(
            "testdata/compat/needs_minor_migration_cors_origins.yaml"
        ))
        .expect("the adapter migrates legacy CORS settings");

        tracing_test::logs_assert(|lines| {
            lines
                .iter()
                .any(|line| line.contains("needs to be upgraded"))
                .then_some(())
                .ok_or_else(|| {
                    "the adapter must report applied migrations like the production loader"
                        .to_string()
                })
        })
        .unwrap();
    }

    /// `router config validate` reports migrations itself, so its parse logs nothing about them.
    #[test]
    fn quiet_migration_logs_nothing_about_what_it_changed() {
        let _guard = tracing_test::dispatcher_guard();

        parse_configuration(
            include_str!("testdata/compat/needs_minor_migration_cors_origins.yaml"),
            ExternalValues::default(),
            Migration::WithinMajorQuietly,
        )
        .expect("the adapter migrates legacy CORS settings");

        tracing_test::logs_assert(|lines| {
            match lines
                .iter()
                .find(|line| line.contains("needs to be upgraded"))
            {
                Some(line) => Err(format!("unexpected migration log: {line}")),
                None => Ok(()),
            }
        })
        .unwrap();
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

    /// A configuration that fails validation may only need `router config upgrade`, so the
    /// operator is told about it.
    #[test]
    fn validation_errors_suggest_router_config_upgrade() {
        let _guard = tracing_test::dispatcher_guard();

        parse("this_key_does_not_exist_anywhere: true\n").expect_err("the key is unknown");

        tracing_test::logs_assert(|lines| {
            lines
                .iter()
                .any(|line| {
                    line.contains("router config upgrade") && line.contains("from-router-v2")
                })
                .then_some(())
                .ok_or_else(|| "expected a hint to run `router config upgrade`".to_string())
        })
        .unwrap();
    }

    /// Every plugin with invalid config is reported, each against its own section.
    #[test]
    fn every_invalid_plugin_is_reported_at_its_section() {
        let text = "# operator comment\ntraffic_shaping:\n  router:\n    timeout: not-a-duration\nsubscription:\n  deduplication:\n    enabled: true\n";

        let error = parse_configuration(text, ExternalValues::default(), Migration::None)
            .expect_err("both plugins' config is invalid")
            .to_string();

        assert!(error.contains("apollo.traffic_shaping"), "{error}");
        assert!(error.contains("apollo.subscription"), "{error}");
        assert!(error.contains("[3:3]"), "{error}");
        assert!(error.contains("[6:3]"), "{error}");
    }

    /// A migrated document that still fails schema validation falls back to validating the
    /// original document, which earlier releases also did, so the diagnostics quote the file.
    #[test]
    fn invalid_migrated_documents_are_reported_against_the_original_text() {
        let text = "# operator comment kept in the snippet\ncors:\n  origins:\n    - \"https://example.com\"\nthis_key_does_not_exist_anywhere: true\n";

        let error = parse(text).expect_err("the unknown key is invalid in either form");

        let error = error.to_string();
        assert!(
            error.contains("this_key_does_not_exist_anywhere"),
            "{error}"
        );
        // The comment makes the unknown key line 5 of the original; the migrated copy has no
        // comment and no `origins`.
        assert!(
            error.contains("[5:1]"),
            "the fallback diagnostics should refer to the original text: {error}"
        );
    }

    /// Expansion errors in a migrated copy also fall back, so their location is in the file too.
    #[test]
    fn expansion_errors_in_migrated_documents_are_reported_against_the_original_text() {
        let _guard = tracing_test::dispatcher_guard();
        // Migration 2045 moves the unresolvable reference under `deduplication.all`, to line 5
        // of the serialized copy; it is on line 4 of the file.
        let text =
            "# operator comment\nsubscription:\n  deduplication:\n    enabled: ${env.MISSING}\n";

        let error = parse_configuration(text, env("PRESENT", "1"), Migration::WithinMajor)
            .expect_err("the reference cannot be resolved")
            .to_string();

        assert!(error.contains("expansion value not present"), "{error}");
        assert!(
            error.contains("[4:"),
            "the diagnostic should refer to the original text: {error}"
        );
        tracing_test::logs_assert(|lines| {
            lines
                .iter()
                .any(|line| line.contains("could not be upgraded automatically"))
                .then_some(())
                .ok_or_else(|| "the fallback must warn that the upgrade failed".to_string())
        })
        .unwrap();
    }
}
