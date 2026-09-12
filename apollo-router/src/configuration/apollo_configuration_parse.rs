//! Parses router configuration through the shared `apollo-configuration` crate.
//!
//! Only tests call this adapter. Cutover blockers include secret redaction through router's
//! draft 7 `#/definitions/` references and normalization of `plugins: null` to an empty map.

use std::sync::Arc;
use std::sync::OnceLock;

use apollo_configuration::ParseYamlOptions;
use serde_json::Value;

use super::Configuration;
use super::ConfigurationError;
use super::expansion::Expansion;
use super::schema::generate_config_schema;

// `Configuration::deserialize` already runs the struct's own cross-field validation, so the
// shared crate's validation hook has nothing further to check.
impl apollo_configuration::Validate for Configuration {}
impl apollo_configuration::Configuration for Configuration {}

/// Uses [`generate_config_schema`] so the shared parser also rejects unknown top-level keys.
#[allow(dead_code)]
pub(crate) fn apollo_configuration_options() -> ParseYamlOptions {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    let schema = SCHEMA
        .get_or_init(|| {
            serde_json::to_value(generate_config_schema())
                .expect("router's configuration schema serializes")
        })
        .clone();
    ParseYamlOptions::default().schema(schema)
}

/// Parses settings, stores the expanded document in `validated_yaml`, and keeps `text` in
/// `raw_yaml`.
///
/// Configure `options` and `expansion` with equivalent external inputs. `options` expands the
/// settings it deserializes; `expansion` supplies `validated_yaml`.
///
/// # Errors
/// Returns errors from YAML parsing, expansion, or the shared parser.
#[allow(dead_code)]
pub(crate) fn parse_via_apollo_configuration(
    text: &str,
    options: &ParseYamlOptions,
    expansion: &Expansion,
) -> Result<Configuration, ConfigurationError> {
    // `serde_yaml` rejects blank input; configuration loading accepts it as an empty mapping.
    let raw: Value = if text.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_yaml::from_str(text).map_err(|error| ConfigurationError::InvalidConfiguration {
            message: "failed to parse yaml",
            error: error.to_string(),
        })?
    };
    let expanded = expansion.expand(&raw)?;
    let mut config: Configuration = options.parse(text)?;
    config.validated_yaml = Some(expanded);
    config.raw_yaml = Some(Arc::from(text));
    Ok(config)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use apollo_configuration::expansion::MapVariables;
    use serde_json::json;

    use super::*;

    #[test]
    fn populates_validated_yaml_and_raw_yaml() {
        let text = include_str!("testdata/compat/current_minimal.yaml");

        let config = parse_via_apollo_configuration(
            text,
            &apollo_configuration_options(),
            &Expansion::builder().build(),
        )
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

        let error = parse_via_apollo_configuration(
            text,
            &apollo_configuration_options(),
            &Expansion::builder().build(),
        )
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
    fn malformed_yaml_returns_a_parse_error() {
        let error = parse_via_apollo_configuration(
            "supergraph: [",
            &apollo_configuration_options(),
            &Expansion::builder().build(),
        )
        .expect_err("the sequence is unterminated");

        assert!(matches!(
            error,
            ConfigurationError::InvalidConfiguration {
                message: "failed to parse yaml",
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
            &apollo_configuration_options(),
            &Expansion::builder().supported_mode("env").build(),
        )
        .expect_err("the expansion kind is unsupported");

        assert!(matches!(
            error,
            ConfigurationError::UnknownExpansionMode { .. }
        ));
        assert!(error.to_string().contains("unsupported.ADDRESS"), "{error}");
    }

    #[test]
    fn an_empty_document_expands_to_the_same_validated_yaml_as_router() {
        let config = parse_via_apollo_configuration(
            "",
            &apollo_configuration_options(),
            &Expansion::builder().build(),
        )
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
        let expansion = Expansion::builder()
            .supported_mode("env")
            .mocked_env_var("COMPAT_TEST_PORT", "4001")
            .build();

        let config = parse_via_apollo_configuration(text, &options, &expansion)
            .expect("both expanders resolve the reference");

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
