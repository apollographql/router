//! Parses router configuration through the shared `apollo-configuration` crate.
//!
//! Only tests call this adapter. Routing production loading through it, including normalization
//! of `plugins: null` to an empty map, remains a separate cutover change.

use std::sync::Arc;
use std::sync::OnceLock;

use apollo_configuration::ParseYamlOptions;
use serde_json::Value;

use super::Configuration;
use super::ConfigurationError;
use super::schema::generate_config_schema;
use super::upgrade::UpgradeMode;
use super::upgrade::upgrade_configuration;

// `Configuration::deserialize` already runs the struct's own cross-field validation, so the
// shared crate's validation hook has nothing further to check.
impl apollo_configuration::Validate for Configuration {}
impl apollo_configuration::Configuration for Configuration {}

/// Router's patched configuration schema, generated once and shared by parsing and tests.
pub(crate) fn router_config_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(generate_config_schema())
            .expect("router's configuration schema serializes")
    })
}

/// Uses [`router_config_schema`] so the shared parser also rejects unknown top-level keys.
#[allow(dead_code)]
pub(crate) fn apollo_configuration_options() -> ParseYamlOptions {
    ParseYamlOptions::default().schema(router_config_schema().clone())
}

/// Retains document values without adding defaults from typed configuration serialization.
/// The supplied Router schema governs validation and expansion coercion.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
struct ExpandedDocument(Value);

impl apollo_configuration::Validate for ExpandedDocument {}
impl apollo_configuration::Configuration for ExpandedDocument {}

/// Applies within-major migrations, then parses typed settings and the retained document
/// using the same shared options. Providers must be stable across both parse calls.
/// `raw_yaml` preserves the exact original text, while diagnostics refer to the migrated copy.
///
/// Unlike the production loader, this adapter rejects invalid migrated input without falling
/// back to the original document. Production loading remains a separate cutover.
///
/// # Errors
/// Returns errors from YAML parsing, migration, or either shared-parser pass.
#[allow(dead_code)]
pub(crate) fn parse_via_apollo_configuration(
    text: &str,
    options: &ParseYamlOptions,
) -> Result<Configuration, ConfigurationError> {
    // Migration serialization must not hide duplicate keys in the original document.
    super::yaml::parse(text)?;
    let raw: Value = if text.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_yaml::from_str(text).map_err(|error| ConfigurationError::InvalidConfiguration {
            message: "failed to parse yaml",
            error: error.to_string(),
        })?
    };
    let major = env!("CARGO_PKG_VERSION_MAJOR")
        .parse()
        .expect("CARGO_PKG_VERSION_MAJOR should be an integer");
    let migrated = upgrade_configuration(&raw, false, UpgradeMode::Minor(major))?;
    let migrated =
        serde_yaml::to_string(&migrated).map_err(|error| ConfigurationError::MigrationFailure {
            error: error.to_string(),
        })?;
    let mut config: Configuration = options.parse(&migrated)?;
    let document: ExpandedDocument = options.parse(&migrated)?;
    config.validated_yaml = Some(document.0);
    config.raw_yaml = Some(Arc::from(text));
    Ok(config)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use apollo_configuration::expansion::MapVariables;
    use serde_json::json;

    use super::*;
    use crate::configuration::expansion::Expansion;

    #[test]
    fn populates_validated_yaml_and_raw_yaml() {
        let text = include_str!("testdata/compat/current_minimal.yaml");

        let config = parse_via_apollo_configuration(text, &apollo_configuration_options())
            .expect("the minimal fixture is valid");

        assert_eq!(
            config.raw_yaml.as_deref(),
            Some(text),
            "raw_yaml must hold the exact pre-expansion input"
        );
        let validated_yaml = config
            .validated_yaml
            .as_ref()
            .expect("validated_yaml must be populated");
        assert!(
            validated_yaml.is_object(),
            "validated_yaml must hold the expanded document, not be left empty: {validated_yaml:?}"
        );
    }

    #[test]
    fn rejected_input_renders_as_a_miette_diagnostic() {
        let text = include_str!("testdata/compat/unknown_top_level_key.yaml");

        let error = parse_via_apollo_configuration(text, &apollo_configuration_options())
            .expect_err("the fixture sets a key the schema does not declare");

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
        let secret = "redis-password-must-not-leak";
        let text = format!(
            "apq:\n  router:\n    cache:\n      redis:\n        urls: [redis://localhost:6379]\n        password: {secret}\n        unexpected: true\n"
        );

        let error = parse_via_apollo_configuration(&text, &apollo_configuration_options())
            .expect_err("the Redis configuration contains an unknown field");
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
        let text = "apq:\n  router:\n    cache:\n      redis:\n        urls: [redis://localhost:6379]\n        password: ${env.ADAPTER_PASSWORD}\n";
        let secret = "adapter-test-password";
        let options =
            apollo_configuration_options().add_variables(MapVariables(HashMap::from([(
                "ADAPTER_PASSWORD".to_string(),
                secret.to_string(),
            )])));
        let config = parse_via_apollo_configuration(text, &options).expect("valid Redis settings");

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
        let error =
            parse_via_apollo_configuration("supergraph: [", &apollo_configuration_options())
                .expect_err("the sequence is unterminated");

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
        let error = parse_via_apollo_configuration(
            "supergraph:\n  listen: ${unsupported.ADDRESS}\n",
            &apollo_configuration_options().add_variables(MapVariables(HashMap::new())),
        )
        .expect_err("the expansion kind is unsupported");

        assert!(matches!(error, ConfigurationError::ApolloConfiguration(_)));
        assert!(error.to_string().contains("unsupported.ADDRESS"), "{error}");
    }

    #[test]
    fn an_empty_document_expands_to_the_same_validated_yaml_as_router() {
        let config = parse_via_apollo_configuration("", &apollo_configuration_options())
            .expect("an empty document is valid");

        let router = crate::configuration::schema::validate_yaml_configuration(
            "",
            Expansion::builder().build(),
            crate::configuration::schema::Mode::NoUpgrade,
        )
        .expect("router's own pipeline accepts an empty document");

        assert_eq!(config.validated_yaml, router.validated_yaml);
    }

    #[test]
    fn validated_yaml_holds_the_expanded_document() {
        let text = "supergraph:\n  listen: 127.0.0.1:${env.COMPAT_TEST_PORT}\n";
        let options =
            apollo_configuration_options().add_variables(MapVariables(HashMap::from([(
                "COMPAT_TEST_PORT".to_string(),
                "4001".to_string(),
            )])));
        let config = parse_via_apollo_configuration(text, &options)
            .expect("the shared parser resolves the reference");

        assert_eq!(
            config.validated_yaml.as_ref().expect("populated")["supergraph"]["listen"],
            json!("127.0.0.1:4001"),
            "validated_yaml must hold the expanded value, not the unexpanded reference"
        );
        assert_eq!(
            config.supergraph.listen.to_string(),
            "http://127.0.0.1:4001",
            "the deserialized configuration must agree with validated_yaml"
        );
    }
}
