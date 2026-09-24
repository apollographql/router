//! Parses router configuration through the shared `apollo-configuration` crate.
//!
//! Startup, hot reload, `Configuration::from_str` and `router config validate` all load
//! configuration here.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_configuration::ConfigError;
use apollo_configuration::ErrorCollector;
use apollo_configuration::ParseYamlOptions;
use apollo_configuration::expansion::LookupError;
use apollo_configuration::expansion::VariableProvider;
use apollo_configuration::provenance::Injection;
use parking_lot::Mutex;
use serde_json::Value;
use serde_json::json;

use super::Configuration;
use super::ConfigurationError;
use super::schema::router_config_schema;
use super::upgrade::UpgradeMode;
use super::upgrade::upgrade_configuration;

/// Reports every plugin whose settings could not be deserialized, at its section of the document.
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

/// Whether parsing first applies the current major version's migrations, as startup and reload
/// do. `router config validate` checks the file as written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Migration {
    WithinMajor,
    None,
}

/// Values the adapter reads from outside the document: expansion providers and injected
/// overrides. The adapter always adds Router's schema itself, because the retained document's
/// own schema accepts anything and would otherwise skip validation and type-directed coercion.
#[derive(Default)]
pub(crate) struct ExternalValues {
    variables: Vec<Box<dyn VariableProvider>>,
    injections: Vec<Injection>,
    replacements: Vec<(Vec<String>, Value)>,
}

impl ExternalValues {
    /// Appends an expansion provider, as [`ParseYamlOptions::add_variables`] does.
    pub(crate) fn add_variables(mut self, provider: impl VariableProvider + 'static) -> Self {
        self.variables.push(Box::new(provider));
        self
    }

    /// Appends injected values, as [`ParseYamlOptions::inject`] does.
    pub(crate) fn inject(mut self, injections: impl IntoIterator<Item = Injection>) -> Self {
        self.injections.extend(injections);
        self
    }

    /// Sets `value` at the dotted `path` before parsing, replacing whatever the document has
    /// there. Unlike an injection, this also replaces a section that has settings in it.
    pub(crate) fn replace(mut self, path: &str, value: Value) -> Self {
        let path = path.split('.').map(str::to_string).collect();
        self.replacements.push((path, value));
        self
    }

    fn apply_replacements(&self, document: &mut Value) {
        replace_all(&self.replacements, document);
    }

    /// Adds the providers and injections to `options`.
    fn add_to(self, options: ParseYamlOptions) -> ParseYamlOptions {
        let options = options.inject(self.injections);
        if self.variables.is_empty() {
            // Without providers the shared parser leaves expansion syntax unchanged.
            return options;
        }
        options.add_variables(ProviderSnapshot {
            providers: self.variables,
            resolved: Default::default(),
        })
    }

    /// Expands `text` with these values but without Router's schema, for testing expansion on
    /// documents that are not router configuration.
    #[cfg(test)]
    pub(crate) fn expand_without_schema(self, text: &str) -> Result<Value, ConfigError> {
        let replacements = self.replacements.clone();
        let ExpandedDocument(mut document) = self
            .add_to(ParseYamlOptions::default())
            .parse::<ExpandedDocument>(text)?;
        replace_all(&replacements, &mut document);
        Ok(document)
    }
}

fn replace_all(replacements: &[(Vec<String>, Value)], document: &mut Value) {
    for (path, value) in replacements {
        replace_at(document, path, value.clone());
    }
}

fn replace_at(document: &mut Value, path: &[String], value: Value) {
    let Some((key, rest)) = path.split_first() else {
        *document = value;
        return;
    };
    if !document.is_object() {
        *document = Value::Object(Default::default());
    }
    let child = document
        .as_object_mut()
        .expect("replaced with an object above")
        .entry(key.clone())
        .or_insert(Value::Null);
    replace_at(child, rest, value);
}

/// Consults each provider in order, as the shared parser does with separately added providers,
/// and remembers each result. Both shared-parser passes then see the same value, even if a file
/// or environment variable changes between them.
struct ProviderSnapshot {
    providers: Vec<Box<dyn VariableProvider>>,
    resolved: Mutex<HashMap<String, Result<String, LookupError>>>,
}

impl VariableProvider for ProviderSnapshot {
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

/// Parses `text` into a configuration whose `validated_yaml` holds the expanded document that
/// configuration-usage telemetry and licence checks read, and whose `raw_yaml` is `text`.
/// Typed settings and the retained document come from two shared-parser passes with Router's
/// schema and the same external values. Each expansion reference is resolved once and reused,
/// so both passes see the same value.
///
/// Replacements, such as the `--dev` defaults, are applied to the document before migration.
///
/// A document that needs no changes is parsed as written, so diagnostics point at the user's
/// lines and YAML aliases keep the shared parser's anchor redaction. A changed document is
/// serialized and parsed instead. If the shared parser rejects the migrated document, whether in
/// expansion, overrides, schema validation, deserialization or cross-field validation, the
/// unmigrated document is parsed in its place, as earlier releases did after a schema failure.
/// Unless replacements changed it, that is the operator's file, where YAML aliases keep the
/// shared parser's anchor redaction.
///
/// Known limitation: an expansion reference anchored on a non-secret field and aliased into a
/// secret field is redacted only in the secret field. A diagnostic about the anchoring field can
/// quote the value the reference resolved to.
///
/// # Errors
/// Returns errors from YAML parsing, migration, expansion, overrides, schema validation,
/// deserialization, or plugin settings.
pub(crate) fn parse_configuration(
    text: &str,
    external: impl Into<ExternalValues>,
    migration: Migration,
) -> Result<Configuration, ConfigurationError> {
    // Migration serialization must not hide duplicate keys in the original document.
    super::yaml::check_duplicate_keys(text)?;
    let file: Value = if text.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_yaml::from_str(text).map_err(|error| ConfigurationError::InvalidConfiguration {
            message: "failed to parse yaml",
            error: error.to_string(),
        })?
    };
    let external = external.into();
    let mut original = file.clone();
    external.apply_replacements(&mut original);
    let migrated = match migration {
        Migration::WithinMajor => {
            upgrade_configuration(&original, true, UpgradeMode::current_minor())?
        }
        Migration::None => original.clone(),
    };
    let options =
        external.add_to(ParseYamlOptions::default().schema(router_config_schema().clone()));
    let parse = |document: &Value| -> Result<_, ConfigurationError> {
        if *document == file {
            return Ok(parse_document(text, &options));
        }
        let serialized = serde_yaml::to_string(document).map_err(|error| {
            ConfigurationError::MigrationFailure {
                error: error.to_string(),
            }
        })?;
        Ok(parse_document(&serialized, &options))
    };

    // Diagnostics for the serialized copy would point at lines the operator never wrote, so any
    // error in it falls back to the supplied text. The fallback still validates that text in
    // full, so it never accepts an invalid document.
    let parsed = match parse(&migrated)? {
        Err(_) if migrated != original => {
            tracing::warn!(
                "Configuration could not be upgraded automatically as it had errors. If you previously used this configuration with Router 1.x, please refer to the migration guide: https://www.apollographql.com/docs/graphos/reference/migration/from-router-v1"
            );
            parse(&original)?
        }
        parsed => parsed,
    };
    let mut config = parsed?;
    config.raw_yaml = Some(Arc::from(text));
    Ok(config)
}

/// Parses typed settings, then the expanded document, with the same options. The retained
/// document shows `plugins: null` as `{}`, which means the same.
fn parse_document(text: &str, options: &ParseYamlOptions) -> Result<Configuration, ConfigError> {
    let mut config: Configuration = options.parse(text)?;
    let ExpandedDocument(mut document) = options.parse(text)?;
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
    fn retained_document_preserves_secret_values_without_serializing_typed_settings() {
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
        let config = parse("plugins: null\n").expect("null plugin settings are accepted");

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
    fn both_passes_see_one_snapshot_of_each_expanded_value() {
        let text = "apq:\n  router:\n    cache:\n      redis:\n        urls: [redis://localhost]\n        password: ${env.PW}\n";
        let reads = Arc::new(AtomicUsize::new(0));
        let external = ExternalValues::default().add_variables(RotatingPassword(reads.clone()));

        let config = parse_configuration(text, external, Migration::WithinMajor)
            .expect("valid Redis settings");

        assert_eq!(reads.load(Ordering::SeqCst), 1, "the provider is read once");
        let redis = config.apq.router.cache.redis.as_ref().unwrap();
        assert_eq!(
            redis.password.as_ref().unwrap().unredact(),
            "password-read-0"
        );
        assert_eq!(
            config.validated_yaml.as_ref().unwrap()["apq"]["router"]["cache"]["redis"]["password"],
            "password-read-0",
            "the retained document must hold the same value as the typed settings"
        );
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
    /// into a secret field. This migrated document fails validation, so the file as written is
    /// reported instead and the shared parser redacts the anchor as in the unmigrated case.
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

    /// Every plugin with invalid settings is reported, each against its own section.
    #[test]
    fn every_invalid_plugin_is_reported_at_its_section() {
        let text = "# operator comment\ntraffic_shaping:\n  router:\n    timeout: not-a-duration\nsubscription:\n  deduplication:\n    enabled: true\n";

        let error = parse_configuration(text, ExternalValues::default(), Migration::None)
            .expect_err("both plugins' settings are invalid")
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
