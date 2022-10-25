//! Configuration schema generation and validation
// This entire file is license key functionality

use std::borrow::Cow;
use std::cmp::Ordering;

use itertools::Itertools;
use jsonschema::Draft;
use jsonschema::JSONSchema;
use schemars::gen::SchemaSettings;
use schemars::schema::RootSchema;

use super::expansion::coerce;
use super::expansion::expand_env_variables;
use super::expansion::Expansion;
use super::plugins;
use super::yaml;
use super::Configuration;
use super::ConfigurationError;
use super::APOLLO_PLUGIN_PREFIX;

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
    let mut root = schema.schema.object.as_mut().expect("schema not generated");
    root.additional_properties = Some(Box::new(schemars::schema::Schema::Bool(false)));
    schema
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
) -> Result<Configuration, ConfigurationError> {
    let defaulted_yaml = if raw_yaml.trim().is_empty() {
        "plugins:".to_string()
    } else {
        raw_yaml.to_string()
    };

    let yaml = &serde_yaml::from_str(&defaulted_yaml).map_err(|e| {
        ConfigurationError::InvalidConfiguration {
            message: "failed to parse yaml",
            error: e.to_string(),
        }
    })?;

    let expanded_yaml = expand_env_variables(yaml, expansion)?;
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
    if let Err(errors) = schema.validate(&expanded_yaml) {
        // Validation failed, translate the errors into something nice for the user
        // We have to reparse the yaml to get the line number information for each error.
        match super::yaml::parse(raw_yaml) {
            Ok(yaml) => {
                let yaml_split_by_lines = raw_yaml.split('\n').collect::<Vec<_>>();

                let errors = errors
                    .enumerate()
                    .filter_map(|(idx, mut e)| {
                        if let Some(element) = yaml.get_element(&e.instance_path) {
                            const NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY: usize = 5;
                            match element {
                                yaml::Value::String(value, marker) => {
                                    let lines = yaml_split_by_lines[0.max(
                                        marker
                                            .line()
                                            .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY),
                                    )
                                        ..marker.line()]
                                        .iter()
                                        .join("\n");

                                    // Replace the value in the error message with the one from the raw config.
                                    // This guarantees that if the env variable contained a secret it won't be leaked.
                                    e.instance = Cow::Owned(coerce(value));

                                    Some(format!(
                                        "{}. {}\n\n{}\n{}^----- {}",
                                        idx + 1,
                                        e.instance_path,
                                        lines,
                                        " ".repeat(0.max(marker.col())),
                                        e
                                    ))
                                }
                                seq_element @ yaml::Value::Sequence(_, m) => {
                                    let (start_marker, end_marker) = (m, seq_element.end_marker());

                                    let offset = 0.max(
                                        start_marker
                                            .line()
                                            .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY),
                                    );
                                    let lines = yaml_split_by_lines[offset..end_marker.line()]
                                        .iter()
                                        .enumerate()
                                        .map(|(idx, line)| {
                                            let real_line = idx + offset;
                                            match real_line.cmp(&start_marker.line()) {
                                                Ordering::Equal => format!("┌ {line}"),
                                                Ordering::Greater => format!("| {line}"),
                                                Ordering::Less => line.to_string(),
                                            }
                                        })
                                        .join("\n");

                                    Some(format!(
                                        "{}. {}\n\n{}\n└-----> {}",
                                        idx + 1,
                                        e.instance_path,
                                        lines,
                                        e
                                    ))
                                }
                                map_value
                                @ yaml::Value::Mapping(current_label, _value, _marker) => {
                                    let (start_marker, end_marker) = (
                                        current_label.as_ref()?.marker.as_ref()?,
                                        map_value.end_marker(),
                                    );
                                    let offset = 0.max(
                                        start_marker
                                            .line()
                                            .saturating_sub(NUMBER_OF_PREVIOUS_LINES_TO_DISPLAY),
                                    );
                                    let lines = yaml_split_by_lines[offset..end_marker.line()]
                                        .iter()
                                        .enumerate()
                                        .map(|(idx, line)| {
                                            let real_line = idx + offset;
                                            match real_line.cmp(&start_marker.line()) {
                                                Ordering::Equal => format!("┌ {line}"),
                                                Ordering::Greater => format!("| {line}"),
                                                Ordering::Less => line.to_string(),
                                            }
                                        })
                                        .join("\n");

                                    Some(format!(
                                        "{}. {}\n\n{}\n└-----> {}",
                                        idx + 1,
                                        e.instance_path,
                                        lines,
                                        e
                                    ))
                                }
                            }
                        } else {
                            None
                        }
                    })
                    .join("\n\n");

                if !errors.is_empty() {
                    return Err(ConfigurationError::InvalidConfiguration {
                        message: "configuration had errors",
                        error: format!("\n{}", errors),
                    });
                }
            }
            Err(e) => {
                // the yaml failed to parse. Just let serde do it's thing.
                tracing::warn!(
                    "failed to parse yaml using marked parser: {}. Falling back to serde validation",
                    e
                );
            }
        }
    }

    let config: Configuration = serde_json::from_value(expanded_yaml)
        .map_err(ConfigurationError::DeserializeConfigError)?;

    // ------------- Check for unknown fields at runtime ----------------
    // We can't do it with the `deny_unknown_fields` property on serde because we are using `flatten`
    let registered_plugins = plugins();
    let apollo_plugin_names: Vec<&str> = registered_plugins
        .keys()
        .filter_map(|n| n.strip_prefix(APOLLO_PLUGIN_PREFIX))
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
        return Err(ConfigurationError::InvalidConfiguration {
            message: "unknown fields",
            error: format!(
                "additional properties are not allowed ('{}' was/were unexpected)",
                unknown_fields.iter().join(", ")
            ),
        });
    }

    Ok(config)
}
