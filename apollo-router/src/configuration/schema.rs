//! Configuration schema generation and validation

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt::Write;

use itertools::Itertools;
use jsonschema::error::ValidationErrorKind;
use jsonschema::Draft;
use jsonschema::JSONSchema;
use schemars::gen::SchemaSettings;
use schemars::schema::RootSchema;
use yaml_rust::scanner::Marker;

use super::expansion::coerce;
use super::expansion::Expansion;
use super::plugins;
use super::yaml;
use super::Configuration;
use super::ConfigurationError;
use super::APOLLO_PLUGIN_PREFIX;
pub(crate) use crate::configuration::upgrade::generate_upgrade;
pub(crate) use crate::configuration::upgrade::upgrade_configuration;

const NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY: usize = 5;

/// Generate a JSON schema for the configuration.
pub(crate) fn generate_config_schema() -> RootSchema {
    let settings = SchemaSettings::draft07().with(|s| {
        s.option_nullable = true;
        s.option_add_null_type = false;
        s.inline_subschemas = true;
    });

    // Manually patch up the schema
    // We don't want to allow unknown fields, but serde doesn't work if we put the annotation on Configuration as the struct has a flattened type.
    // It's fine to just add it here.
    let gen = settings.into_generator();
    let mut schema = gen.into_root_schema_for::<Configuration>();
    let root = schema.schema.object.as_mut().expect("schema not generated");
    root.additional_properties = Some(Box::new(schemars::schema::Schema::Bool(false)));
    schema
}

#[derive(Eq, PartialEq)]
pub(crate) enum Mode {
    Upgrade,

    // This is used only in testing to ensure that we don't allow old config in our tests.
    #[cfg(test)]
    NoUpgrade,
}

/// Validate config yaml against the generated json schema.
/// This is a tricky problem, and the solution here is by no means complete.
/// In the case that validation cannot be performed then it will let serde validate as normal. The
/// goal is to give a good enough experience until more time can be spent making this better,
///
/// The validation sequence is:
/// 1. Parse the config into yaml
/// 2. Create the json schema
/// 3. Expand env variables
/// 3. Validate the yaml against the json schema.
/// 4. Convert the json paths from the error messages into nice error snippets. Makes sure to use the values from the original source document to prevent leaks of secrets etc.
///
/// There may still be serde validation issues later.
///
pub(crate) fn validate_yaml_configuration(
    raw_yaml: &str,
    expansion: Expansion,
    migration: Mode,
) -> Result<Configuration, ConfigurationError> {
    let defaulted_yaml = if raw_yaml.trim().is_empty() {
        "plugins:".to_string()
    } else {
        raw_yaml.to_string()
    };

    let mut yaml = serde_yaml::from_str(&defaulted_yaml).map_err(|e| {
        ConfigurationError::InvalidConfiguration {
            message: "failed to parse yaml",
            error: e.to_string(),
        }
    })?;
    let schema = serde_json::to_value(generate_config_schema()).map_err(|e| {
        ConfigurationError::InvalidConfiguration {
            message: "failed to parse schema",
            error: e.to_string(),
        }
    })?;
    let schema = JSONSchema::options()
        .with_draft(Draft::Draft7)
        .compile(&schema)
        .map_err(|e| ConfigurationError::InvalidConfiguration {
            message: "failed to compile schema",
            error: e.to_string(),
        })?;

    if migration == Mode::Upgrade {
        let upgraded = upgrade_configuration(&yaml, true)?;
        let expanded_yaml = expansion.expand(&upgraded)?;
        if schema.validate(&expanded_yaml).is_ok() {
            yaml = upgraded;
        } else {
            tracing::warn!("configuration could not be upgraded automatically as it had errors")
        }
    }

    let expanded_yaml = expansion.expand(&yaml)?;
    let parsed_yaml = super::yaml::parse(raw_yaml)?;
    if let Err(errors_it) = schema.validate(&expanded_yaml) {
        // Validation failed, translate the errors into something nice for the user
        // We have to reparse the yaml to get the line number information for each error.
        let yaml_split_by_lines = raw_yaml.split('\n').collect::<Vec<_>>();

        let mut errors = String::new();

        for (idx, mut e) in errors_it.enumerate() {
            if let Some(element) = parsed_yaml.get_element(&e.instance_path) {
                match element {
                    yaml::Value::String(value, marker) => {
                        let start_marker = marker;
                        let end_marker = marker;
                        let offset = start_marker
                            .line()
                            .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY);

                        let lines = yaml_split_by_lines[offset..end_marker.line()]
                            .iter()
                            .map(|line| format!("  {line}"))
                            .join("\n");

                        // Replace the value in the error message with the one from the raw config.
                        // This guarantees that if the env variable contained a secret it won't be leaked.
                        e.instance = Cow::Owned(coerce(value));

                        let _ = write!(
                            &mut errors,
                            "{}. at line {}\n\n{}\n{}^----- {}\n\n",
                            idx + 1,
                            start_marker.line(),
                            lines,
                            " ".repeat(2 + marker.col()),
                            e
                        );
                    }
                    seq_element @ yaml::Value::Sequence(_, m) => {
                        let (start_marker, end_marker) = (m, seq_element.end_marker());

                        let lines = context_lines(&yaml_split_by_lines, start_marker, end_marker);

                        let _ = write!(
                            &mut errors,
                            "{}. at line {}\n\n{}\n└-----> {}\n\n",
                            idx + 1,
                            start_marker.line(),
                            lines,
                            e
                        );
                    }
                    map_value @ yaml::Value::Mapping(current_label, map, marker) => {
                        // workaround because ValidationErrorKind is not Clone
                        let unexpected_opt = match &e.kind {
                            ValidationErrorKind::AdditionalProperties { unexpected } => {
                                Some(unexpected.clone())
                            }
                            _ => None,
                        };

                        if let Some(unexpected) = unexpected_opt {
                            for key in unexpected {
                                if let Some((label, value)) =
                                    map.iter().find(|(label, _)| label.name == key)
                                {
                                    let (start_marker, end_marker) = (
                                        label.marker.as_ref().unwrap_or(marker),
                                        value.end_marker(),
                                    );

                                    let lines = context_lines(
                                        &yaml_split_by_lines,
                                        start_marker,
                                        end_marker,
                                    );

                                    e.kind = ValidationErrorKind::AdditionalProperties {
                                        unexpected: vec![key.clone()],
                                    };

                                    let _ = write!(
                                        &mut errors,
                                        "{}. at line {}\n\n{}\n└-----> {}\n\n",
                                        idx + 1,
                                        start_marker.line(),
                                        lines,
                                        e
                                    );
                                }
                            }
                        } else {
                            let (start_marker, end_marker) = (
                                current_label
                                    .as_ref()
                                    .and_then(|label| label.marker.as_ref())
                                    .unwrap_or(marker),
                                map_value.end_marker(),
                            );

                            let lines =
                                context_lines(&yaml_split_by_lines, start_marker, end_marker);

                            let _ = write!(
                                &mut errors,
                                "{}. at line {}\n\n{}\n└-----> {}\n\n",
                                idx + 1,
                                start_marker.line(),
                                lines,
                                e
                            );
                        }
                    }
                }
            }
        }

        if !errors.is_empty() {
            return Err(ConfigurationError::InvalidConfiguration {
                message: "configuration had errors",
                error: format!("\n{errors}"),
            });
        }
    }

    let mut config: Configuration = serde_json::from_value(expanded_yaml.clone())
        .map_err(ConfigurationError::DeserializeConfigError)?;

    // ------------- Check for unknown fields at runtime ----------------
    // We can't do it with the `deny_unknown_fields` property on serde because we are using `flatten`
    let registered_plugins = plugins();
    let apollo_plugin_names: Vec<&str> = registered_plugins
        .filter_map(|factory| factory.name.strip_prefix(APOLLO_PLUGIN_PREFIX))
        .collect();
    let unknown_fields: Vec<&String> = config
        .apollo_plugins
        .plugins
        .keys()
        .filter(|ap_name| {
            let ap_name = ap_name.as_str();
            ap_name != "server" && ap_name != "plugins" && !apollo_plugin_names.contains(&ap_name)
        })
        .collect();

    if !unknown_fields.is_empty() {
        // If you end up here while contributing,
        // It might mean you forgot to update
        // `impl<'de> serde::Deserialize<'de> for Configuration
        // In `/apollo-router/src/configuration/mod.rs`
        return Err(ConfigurationError::InvalidConfiguration {
            message: "unknown fields",
            error: format!(
                "additional properties are not allowed ('{}' was/were unexpected)",
                unknown_fields.iter().join(", ")
            ),
        });
    }
    config.validated_yaml = Some(expanded_yaml);
    Ok(config)
}

fn context_lines(
    yaml_split_by_lines: &[&str],
    start_marker: &Marker,
    end_marker: &Marker,
) -> String {
    let offset = start_marker
        .line()
        .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY);

    yaml_split_by_lines[offset..end_marker.line()]
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            let real_line = idx + offset + 1;
            match real_line.cmp(&start_marker.line()) {
                Ordering::Equal => format!("┌ {line}"),
                Ordering::Greater => format!("| {line}"),
                Ordering::Less => format!("  {line}"),
            }
        })
        .join("\n")
}
