use std::fmt::Write as _;
use std::str::FromStr;

use itertools::Itertools;
use proteus::Parser;
use proteus::TransformBuilder;
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::Value;
use tracing_core::Level;

use crate::error::ConfigurationError;

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
    Add {
        path: String,
        name: String,
        value: Value,
    },
    Delete {
        path: String,
    },
    Copy {
        from: String,
        to: String,
    },
    Move {
        from: String,
        to: String,
    },
    Change {
        path: String,
        from: Value,
        to: Value,
    },
    /// Don't migrate anything, just log a better message before the parsing error.
    /// It can be useful when you're moving a feature from experimental to GA and it is not backward compatible
    Log {
        path: String,
        level: String,
        log: String,
        #[serde(default)]
        condition: LogCondition,
    },
}

/// Filter applied to values selected by the JSONPath in a `Log` action.
#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum LogCondition {
    /// Log when the path returns any results (default).
    #[default]
    NonEmpty,
    /// Only log when at least one matched value is a plain string.
    /// Used to detect deprecated untagged-string selector variants (e.g. `Static(String)`).
    IsString,
}

const REMOVAL_VALUE: &str = "__PLEASE_DELETE_ME";
const REMOVAL_EXPRESSION: &str = r#"const("__PLEASE_DELETE_ME")"#;

#[derive(Debug, Clone, Copy)]
pub(crate) enum UpgradeMode {
    /// Upgrade using migrations for major version (eg: from router 1.x to router 2.x)
    Major,
    /// Upgrade using migrations for minor version (eg: from router 2.x to router 2.y)
    Minor,
}

pub(crate) fn upgrade_configuration(
    config: &serde_json::Value,
    log_warnings: bool,
    upgrade_mode: UpgradeMode,
) -> Result<serde_json::Value, super::ConfigurationError> {
    const CURRENT_MAJOR_VERSION: &str = env!("CARGO_PKG_VERSION_MAJOR");
    // Transformers are loaded from a file and applied in order
    let mut migrations: Vec<Migration> = Vec::new();
    let files = Asset::iter().sorted().filter(|f| {
        if matches!(upgrade_mode, UpgradeMode::Major) {
            f.ends_with(".yaml")
        } else {
            f.ends_with(".yaml") && f.starts_with(CURRENT_MAJOR_VERSION)
        }
    });
    for filename in files {
        if let Some(migration) = Asset::get(&filename) {
            let parsed_migration = serde_yaml::from_slice(&migration.data).map_err(|error| {
                ConfigurationError::MigrationFailure {
                    error: format!("Failed to parse migration {filename}: {error}"),
                }
            })?;
            migrations.push(parsed_migration);
        }
    }

    let mut config = config.clone();
    let mut effective_descriptions: Vec<String> = Vec::new();

    for migration in &migrations {
        let new_config = apply_migration(&config, migration)?;

        // If the config has been modified by the migration then let the user know
        if new_config != config {
            effective_descriptions.push(migration.description.clone());
        }

        // Get ready for the next migration
        config = new_config;
    }

    // Rust-side migrations for transformations that cannot be expressed as
    // YAML actions (e.g. composite keys built from two dynamic map keys).
    // These run after the YAML migrations so any preceding renames (e.g.
    // `preview_connectors` → `connectors`) are already in place.
    if matches!(upgrade_mode, UpgradeMode::Major)
        && migrate_connectors_subgraphs_to_sources(&mut config)
    {
        effective_descriptions.push(
            "Apollo Connectors `connectors.subgraphs` configuration has been replaced by `connectors.sources` keyed by `<subgraph>.<source>`".to_string(),
        );
    }

    // Custom migration: wrap headers operation lists under an `operations` key.
    // Handled in Rust rather than a proteus YAML action because the source path
    // is a prefix of the destination path and subgraph names are dynamic.
    let migrated = migrate_headers_operations(config.clone());
    if migrated != config {
        if log_warnings {
            tracing::warn!(
                "`headers.all.request`, per-subgraph equivalents, and \
                 `headers.connector.{{all,sources.*}}.request` now require an `operations` key \
                 wrapping the list of propagation rules. The router has applied this change \
                 automatically. Please update your configuration file."
            );
        }
        config = migrated;
    }

    if !effective_descriptions.is_empty() && log_warnings {
        tracing::error!(
            "router configuration contains unsupported options and needs to be upgraded to run the router: \n\n{}\n\n",
            effective_descriptions
                .iter()
                .enumerate()
                .map(|(idx, m)| format!("  {}. {}", idx + 1, m))
                .join("\n\n")
        );
    }
    Ok(config)
}

/// Collapse `connectors.subgraphs.<sub>.sources.<src>` entries into
/// `connectors.sources["<sub>.<src>"]`. Returns true only if at least one
/// source was actually migrated.
///
/// Precedence matches the deprecated runtime: the old-shape subgraph-level
/// `$config` overwrites any source-level `$config` that may also be present,
/// because the deprecated `apply_config` branch unconditionally re-assigned
/// `connector.config` from the subgraph block. If a new-shape entry already
/// exists for the composite key, the old-shape values are dropped on the floor
/// (the existing new-shape entry is preserved). A subgraph entry with
/// `$config` but no `sources` cannot be expressed in the new shape, so its
/// `$config` is dropped — under the new model, `$config` is per-source.
fn migrate_connectors_subgraphs_to_sources(config: &mut Value) -> bool {
    let Some(connectors) = config.get_mut("connectors").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(Value::Object(subgraphs)) = connectors.remove("subgraphs") else {
        return false;
    };

    let sources_entry = connectors
        .entry("sources".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    let Value::Object(sources) = sources_entry else {
        // `sources` is present but not an object — leave it alone and put
        // `subgraphs` back so the validation error surfaces the actual problem.
        connectors.insert("subgraphs".to_string(), Value::Object(subgraphs));
        return false;
    };

    let mut migrated_any = false;
    for (subgraph_name, subgraph_value) in subgraphs {
        let Value::Object(mut subgraph_obj) = subgraph_value else {
            continue;
        };
        let parent_config = subgraph_obj.remove("$config");
        let Some(Value::Object(subgraph_sources)) = subgraph_obj.remove("sources") else {
            continue;
        };
        for (source_name, source_value) in subgraph_sources {
            let composite_key = format!("{subgraph_name}.{source_name}");
            if sources.contains_key(&composite_key) {
                continue;
            }
            let Value::Object(mut source_obj) = source_value else {
                sources.insert(composite_key, source_value);
                migrated_any = true;
                continue;
            };
            if let Some(parent_config) = &parent_config {
                source_obj.insert("$config".to_string(), parent_config.clone());
            }
            sources.insert(composite_key, Value::Object(source_obj));
            migrated_any = true;
        }
    }

    migrated_any
}

fn apply_migration(config: &Value, migration: &Migration) -> Result<Value, ConfigurationError> {
    let mut transformer_builder = TransformBuilder::default();
    //We always copy the entire doc to the destination first
    transformer_builder = transformer_builder.add_action(Parser::parse("", "")?);
    for action in &migration.actions {
        match action {
            Action::Add { path, name, value } => {
                if !jsonpath_lib::select(config, &format!("$.{path}"))
                    .unwrap_or_default()
                    .is_empty()
                    && jsonpath_lib::select(config, &format!("$.{path}.{name}"))
                        .unwrap_or_default()
                        .is_empty()
                {
                    transformer_builder = transformer_builder.add_action(Parser::parse(
                        &format!(r#"const({value})"#),
                        &format!("{path}.{name}"),
                    )?);
                }
            }
            Action::Delete { path } => {
                if !jsonpath_lib::select(config, &format!("$.{path}"))
                    .unwrap_or_default()
                    .is_empty()
                {
                    // Deleting isn't actually supported by protus so we add a magic value to delete later
                    transformer_builder =
                        transformer_builder.add_action(Parser::parse(REMOVAL_EXPRESSION, path)?);
                }
            }
            Action::Copy { from, to } => {
                if !jsonpath_lib::select(config, &format!("$.{from}"))
                    .unwrap_or_default()
                    .is_empty()
                {
                    transformer_builder = transformer_builder.add_action(Parser::parse(from, to)?);
                }
            }
            Action::Move { from, to } => {
                if !jsonpath_lib::select(config, &format!("$.{from}"))
                    .unwrap_or_default()
                    .is_empty()
                {
                    transformer_builder = transformer_builder.add_action(Parser::parse(from, to)?);
                    // Deleting isn't actually supported by protus so we add a magic value to delete later
                    transformer_builder =
                        transformer_builder.add_action(Parser::parse(REMOVAL_EXPRESSION, from)?);
                }
            }
            Action::Change { path, from, to } => {
                // We query the value directly (`$.<path>`) rather than using a root-level
                // filter expression (`$[?(@.<path> == <from>)]`) — jsonpath_lib's filter
                // form does not support traversing paths more than two levels deep.
                if jsonpath_lib::select(config, &format!("$.{path}"))
                    .unwrap_or_default()
                    .into_iter()
                    .any(|v| v == from)
                {
                    transformer_builder = transformer_builder
                        .add_action(Parser::parse(&format!(r#"const({to})"#), path)?);
                }
            }
            Action::Log {
                path,
                level,
                log,
                condition,
            } => {
                let level = Level::from_str(level).map_err(migration_failure_error)?;

                let values = jsonpath_lib::select(config, &format!("$.{path}")).unwrap_or_default();
                let should_log = match condition {
                    LogCondition::NonEmpty => !values.is_empty(),
                    LogCondition::IsString => values.iter().any(|v| v.is_string()),
                };
                if should_log {
                    match level {
                        Level::INFO => tracing::info!("{log}"),
                        Level::ERROR => tracing::error!("{log}"),
                        Level::WARN => tracing::warn!("{log}"),
                        Level::TRACE => tracing::trace!("{log}"),
                        Level::DEBUG => tracing::debug!("{log}"),
                    }
                }
            }
        }
    }
    let transformer = transformer_builder.build()?;
    let mut new_config = transformer.apply(config)?;

    // Now we need to clean up elements that should be deleted.
    cleanup(&mut new_config);

    Ok(new_config)
}

/// Used for upgrade command
pub(crate) fn generate_upgrade(config: &str, diff: bool) -> Result<String, ConfigurationError> {
    let parsed_config =
        serde_yaml::from_str(config).map_err(|error| ConfigurationError::MigrationFailure {
            error: format!("Failed to parse config: {error}"),
        })?;
    let upgraded_config = upgrade_configuration(&parsed_config, true, UpgradeMode::Major)?;
    let upgraded_config = serde_yaml::to_string(&upgraded_config).map_err(|error| {
        ConfigurationError::MigrationFailure {
            error: format!("Failed to serialize upgraded config: {error}"),
        }
    })?;
    generate_upgrade_output(config, &upgraded_config, diff)
}

pub(crate) fn generate_upgrade_output(
    config: &str,
    upgraded_config: &str,
    diff: bool,
) -> Result<String, ConfigurationError> {
    // serde doesn't deal with whitespace and comments, these are lost in the upgrade process, so instead we try and preserve this in the diff.
    // It's not ideal, and ideally the upgrade process should work on a DOM that is not serde, but for now we just make a best effort to preserve comments and whitespace.
    // There absolutely are issues where comments will get stripped, but the output should be `correct`.
    let mut output = String::new();

    let diff_result = diff::lines(config, upgraded_config);

    for diff_line in diff_result {
        match diff_line {
            diff::Result::Left(l) => {
                let trimmed = l.trim();
                if !trimmed.starts_with('#') && !trimmed.is_empty() {
                    if diff {
                        writeln!(output, "-{l}").map_err(migration_failure_error)?;
                    }
                } else if diff {
                    writeln!(output, " {l}").map_err(migration_failure_error)?;
                } else {
                    writeln!(output, "{l}").map_err(migration_failure_error)?;
                }
            }
            diff::Result::Both(l, _) => {
                if diff {
                    writeln!(output, " {l}").map_err(migration_failure_error)?;
                } else {
                    writeln!(output, "{l}").map_err(migration_failure_error)?;
                }
            }
            diff::Result::Right(r) => {
                let trimmed = r.trim();
                if trimmed != "---" && !trimmed.is_empty() {
                    if diff {
                        writeln!(output, "+{r}").map_err(migration_failure_error)?;
                    } else {
                        writeln!(output, "{r}").map_err(migration_failure_error)?;
                    }
                }
            }
        }
    }
    Ok(output)
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

/// Migrate the headers plugin config from the old flat-list shape to the new wrapped shape.
///
/// Old: `headers.all.request: [list of operations]`
/// New: `headers.all.request.operations: [list of operations]`
///
/// Can't be expressed as a proteus YAML action because the source path is a prefix of the
/// destination path, and subgraph names are dynamic so they can't be addressed with static
/// dot-notation paths.
fn migrate_headers_operations(mut config: Value) -> Value {
    let Some(headers) = config.get_mut("headers") else {
        return config;
    };

    if let Some(all) = headers.get_mut("all") {
        wrap_operations_if_array(all, "request");
    }

    if let Some(Value::Object(subgraphs)) = headers.get_mut("subgraphs") {
        for sg in subgraphs.values_mut() {
            wrap_operations_if_array(sg, "request");
        }
    }

    // Connector header config has the same flat→wrapped shape change.
    if let Some(connector) = headers.get_mut("connector") {
        if let Some(all) = connector.get_mut("all") {
            wrap_operations_if_array(all, "request");
        }
        if let Some(Value::Object(sources)) = connector.get_mut("sources") {
            for src in sources.values_mut() {
                wrap_operations_if_array(src, "request");
            }
        }
    }

    config
}

/// If `parent[key]` is an array, replace it with `{ "operations": <array> }`.
fn wrap_operations_if_array(parent: &mut Value, key: &str) {
    if matches!(parent.get(key), Some(Value::Array(_))) {
        let arr = parent[key].take();
        parent[key] = serde_json::json!({ "operations": arr });
    }
}

fn migration_failure_error<T: std::fmt::Display>(error: T) -> ConfigurationError {
    ConfigurationError::MigrationFailure {
        error: error.to_string(),
    }
}

#[cfg(test)]
mod test {
    use serde_json::Value;
    use serde_json::json;

    use crate::configuration::upgrade::Action;
    use crate::configuration::upgrade::Migration;
    use crate::configuration::upgrade::apply_migration;
    use crate::configuration::upgrade::generate_upgrade_output;
    use crate::configuration::upgrade::migrate_connectors_subgraphs_to_sources;

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
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Delete {
                        path: "obj.field1".to_string()
                    })
                    .description("delete field1")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn delete_array_element() {
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Delete {
                        path: "arr[0]".to_string()
                    })
                    .description("delete arr[0]")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn move_field() {
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Move {
                        from: "obj.field1".to_string(),
                        to: "new.obj.field1".to_string()
                    })
                    .description("move field1")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn add_field() {
        // This one won't add the field because `obj.field1` already exists
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Add {
                        path: "obj".to_string(),
                        name: "field1".to_string(),
                        value: 25.into()
                    })
                    .description("add field1")
                    .build(),
            )
            .expect("expected successful migration")
        );

        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Add {
                        path: "obj".to_string(),
                        name: "field3".to_string(),
                        value: 42.into()
                    })
                    .description("add field3")
                    .build(),
            )
            .expect("expected successful migration")
        );

        // This one won't add the field because `unexistent` doesn't exist, we don't add parent structure
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Add {
                        path: "unexistent".to_string(),
                        name: "field".to_string(),
                        value: 1.into()
                    })
                    .description("add field3")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn move_non_existent_field() {
        insta::assert_json_snapshot!(
            apply_migration(
                &json!({"should": "stay"}),
                &Migration::builder()
                    .action(Action::Move {
                        from: "obj.field1".to_string(),
                        to: "new.obj.field1".to_string()
                    })
                    .description("move field1")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn move_array_element() {
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Move {
                        from: "arr[0]".to_string(),
                        to: "new.arr[0]".to_string()
                    })
                    .description("move arr[0]")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn copy_field() {
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Copy {
                        from: "obj.field1".to_string(),
                        to: "new.obj.field1".to_string()
                    })
                    .description("copy field1")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn copy_array_element() {
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Copy {
                        from: "arr[0]".to_string(),
                        to: "new.arr[0]".to_string()
                    })
                    .description("copy arr[0]")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn diff_upgrade_output() {
        insta::assert_snapshot!(
            generate_upgrade_output(
                "changed: bar\nstable: 1.0\ndeleted: gone",
                "changed: bif\nstable: 1.0\nadded: new",
                true
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn upgrade_output() {
        insta::assert_snapshot!(
            generate_upgrade_output(
                "changed: bar\nstable: 1.0\ndeleted: gone",
                "changed: bif\nstable: 1.0\nadded: new",
                false
            )
            .expect("expected successful migration")
        );
    }

    #[test]
    fn connectors_subgraphs_collapses_into_sources() {
        let mut config = json!({
            "connectors": {
                "subgraphs": {
                    "sub_a": {
                        "$config": { "api_key": "secret" },
                        "sources": {
                            "src_1": { "override_url": "http://one" },
                            "src_2": { "override_url": "http://two", "max_requests_per_operation": 5 }
                        }
                    }
                }
            }
        });
        assert!(migrate_connectors_subgraphs_to_sources(&mut config));
        insta::assert_json_snapshot!(config);
    }

    #[test]
    fn connectors_subgraphs_merges_with_existing_sources_and_existing_wins() {
        let mut config = json!({
            "connectors": {
                "sources": {
                    "sub_a.src_1": { "override_url": "http://new" }
                },
                "subgraphs": {
                    "sub_a": {
                        "sources": {
                            "src_1": { "override_url": "http://old" },
                            "src_2": { "override_url": "http://two" }
                        }
                    }
                }
            }
        });
        assert!(migrate_connectors_subgraphs_to_sources(&mut config));
        insta::assert_json_snapshot!(config);
    }

    #[test]
    fn connectors_subgraphs_no_op_when_absent() {
        let mut config = json!({
            "connectors": {
                "sources": { "sub_a.src_1": { "override_url": "http://one" } }
            }
        });
        assert!(!migrate_connectors_subgraphs_to_sources(&mut config));
    }

    #[test]
    fn connectors_subgraphs_subgraph_config_overwrites_source_config() {
        // Matches the deprecated runtime: when both subgraph-level and
        // source-level `$config` are defined in the same connector, the
        // subgraph-level value wins because the deprecated branch ran second
        // and unconditionally re-assigned connector.config.
        let mut config = json!({
            "connectors": {
                "subgraphs": {
                    "sub_a": {
                        "$config": { "key": "from-subgraph" },
                        "sources": {
                            "src_1": { "$config": { "key": "from-source" } }
                        }
                    }
                }
            }
        });
        assert!(migrate_connectors_subgraphs_to_sources(&mut config));
        insta::assert_json_snapshot!(config);
    }

    #[test]
    fn connectors_subgraphs_empty_is_no_op() {
        // An empty `subgraphs: {}` is removed from the config (so serde's
        // deny_unknown_fields does not trip on it) but does not count as an
        // effective migration — the function returns false so no log fires.
        let mut config = json!({
            "connectors": {
                "subgraphs": {},
                "sources": { "sub_a.src_1": { "override_url": "http://one" } }
            }
        });
        assert!(!migrate_connectors_subgraphs_to_sources(&mut config));
        // empty `subgraphs:` is stripped so validation can proceed
        assert!(config["connectors"].get("subgraphs").is_none());
    }

    #[test]
    fn connectors_subgraphs_subgraph_without_sources_is_no_op() {
        // A subgraph entry that only has $config (no .sources map) cannot be
        // expressed in the new shape — there's no source key to attach the
        // $config to. The function returns false in this case.
        let mut config = json!({
            "connectors": {
                "subgraphs": {
                    "sub_a": { "$config": { "key": "lost" } }
                }
            }
        });
        assert!(!migrate_connectors_subgraphs_to_sources(&mut config));
    }

    #[test]
    fn change_field() {
        insta::assert_json_snapshot!(
            apply_migration(
                &source_doc(),
                &Migration::builder()
                    .action(Action::Change {
                        path: "obj.field1".to_string(),
                        from: Value::Number(1u64.into()),
                        to: Value::String("a".into()),
                    })
                    .description("change field1")
                    .build(),
            )
            .expect("expected successful migration")
        );
    }
}
