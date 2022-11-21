use crate::error::ConfigurationError;
use itertools::Itertools;
use proteus::{Parser, TransformBuilder};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::Value;

#[derive(RustEmbed)]
#[folder = "src/configuration/migrations"]
struct Asset;

#[derive(Deserialize, buildstructor::Builder)]
struct Migration {
    description: String,
    actions: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Action {
    Delete { path: String },
    Copy { from: String, to: String },
    Move { from: String, to: String },
}

const REMOVAL_VALUE: &str = "__PLEASE_DELETE_ME";
const REMOVAL_EXPRESSION: &str = r#"const("__PLEASE_DELETE_ME")"#;

pub(crate) fn upgrade_configuration(
    mut config: serde_json::Value,
    log_warnings: bool,
) -> Result<serde_json::Value, super::ConfigurationError> {
    // Transformers are loaded from a file and applied in order
    let migrations: Vec<Migration> = Asset::iter()
        .sorted()
        .filter(|filename| filename.ends_with(".yaml"))
        .map(|filename| Asset::get(&filename).expect("migration must exist").data)
        .map(|data| serde_yaml::from_slice(&data).expect("migration must be valid"))
        .collect();

    for migration in migrations {
        let new_config = apply_migration(&config, &migration)?;

        // If the config has been modified by the migration then let the user know
        if log_warnings && new_config != config {
            tracing::warn!("router configuration contains deprecated options: {}. This will become an error in the future", migration.description);
        }

        // Get ready for the next migration
        config = new_config;
    }
    Ok(config)
}

fn apply_migration(config: &Value, migration: &Migration) -> Result<Value, ConfigurationError> {
    let mut transformer_builder = TransformBuilder::default();
    //We always copy the entire doc to the destination first
    transformer_builder =
        transformer_builder.add_action(Parser::parse("", "").expect("migration must be valid"));
    for action in &migration.actions {
        match action {
            Action::Delete { path } => {
                // Deleting isn't actually supported by protus so we add a magic value to delete later
                transformer_builder = transformer_builder.add_action(
                    Parser::parse(REMOVAL_EXPRESSION, &path).expect("migration must be valid"),
                );
            }
            Action::Copy { from, to } => {
                transformer_builder = transformer_builder
                    .add_action(Parser::parse(&from, &to).expect("migration must be valid"));
            }
            Action::Move { from, to } => {
                transformer_builder = transformer_builder
                    .add_action(Parser::parse(&from, &to).expect("migration must be valid"));
                // Deleting isn't actually supported by protus so we add a magic value to delete later
                transformer_builder = transformer_builder.add_action(
                    Parser::parse(REMOVAL_EXPRESSION, &from).expect("migration must be valid"),
                );
            }
        }
    }
    let transformer = transformer_builder
        .build()
        .expect("transformer for migration must be valid");
    let mut new_config =
        transformer
            .apply(&config)
            .map_err(|e| ConfigurationError::MigrationFailure {
                error: e.to_string(),
            })?;

    // Now we need to clean up elements that should be deleted.
    cleanup(&mut new_config);

    Ok(new_config)
}

fn cleanup(value: &mut Value) {
    match value {
        Value::Null => {}
        Value::Bool(_) => {}
        Value::Number(_) => {}
        Value::String(_) => {}
        Value::Array(a) => {
            a.retain(|v| &Value::String(REMOVAL_VALUE.to_string()) != v);
            for value in a {
                cleanup(value);
            }
        }
        Value::Object(o) => {
            o.retain(|_, v| &Value::String(REMOVAL_VALUE.to_string()) != v);
            for value in o.values_mut() {
                cleanup(value);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use crate::configuration::upgrade::{apply_migration, Action, Migration};
    use serde_json::{json, Value};

    fn source_doc() -> Value {
        json!( {
          "obj": {
                "field1": 1,
                "field2": 2
            },
          "arr": [
                "v1",
                "v2"
            ]
        })
    }

    #[test]
    fn delete_field() {
        insta::assert_json_snapshot!(apply_migration(
            &source_doc(),
            &Migration::builder()
                .action(Action::Delete {
                    path: "obj.field1".to_string()
                })
                .description("delete field1")
                .build(),
        )
        .expect("expected successful migration"));
    }

    #[test]
    fn delete_array_element() {
        insta::assert_json_snapshot!(apply_migration(
            &source_doc(),
            &Migration::builder()
                .action(Action::Delete {
                    path: "arr[0]".to_string()
                })
                .description("delete arr[0]")
                .build(),
        )
        .expect("expected successful migration"));
    }

    #[test]
    fn move_field() {
        insta::assert_json_snapshot!(apply_migration(
            &source_doc(),
            &Migration::builder()
                .action(Action::Move {
                    from: "obj.field1".to_string(),
                    to: "new.obj.field1".to_string()
                })
                .description("move field1")
                .build(),
        )
        .expect("expected successful migration"));
    }

    #[test]
    fn move_array_element() {
        insta::assert_json_snapshot!(apply_migration(
            &source_doc(),
            &Migration::builder()
                .action(Action::Move {
                    from: "arr[0]".to_string(),
                    to: "new.arr[0]".to_string()
                })
                .description("move arr[0]")
                .build(),
        )
        .expect("expected successful migration"));
    }

    #[test]
    fn copy_field() {
        insta::assert_json_snapshot!(apply_migration(
            &source_doc(),
            &Migration::builder()
                .action(Action::Copy {
                    from: "obj.field1".to_string(),
                    to: "new.obj.field1".to_string()
                })
                .description("copy field1")
                .build(),
        )
        .expect("expected successful migration"));
    }

    #[test]
    fn copy_array_element() {
        insta::assert_json_snapshot!(apply_migration(
            &source_doc(),
            &Migration::builder()
                .action(Action::Copy {
                    from: "arr[0]".to_string(),
                    to: "new.arr[0]".to_string()
                })
                .description("copy arr[0]")
                .build(),
        )
        .expect("expected successful migration"));
    }
}
