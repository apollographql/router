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
    /// Upgrade using migrations for a given minor version (eg: from router 2.x to router 2.y)
    Minor(i64),
}

pub(crate) fn upgrade_configuration(
    config: &serde_json::Value,
    log_warnings: bool,
    upgrade_mode: UpgradeMode,
) -> Result<serde_json::Value, super::ConfigurationError> {
    // Transformers are loaded from a file and applied in order
    let mut migrations: Vec<Migration> = Vec::new();
    let files = Asset::iter().sorted().filter(|f| match upgrade_mode {
        UpgradeMode::Major => f.ends_with(".yaml"),
        UpgradeMode::Minor(major_version) => {
            let major_version = major_version.to_string();
            f.ends_with(".yaml") && f.starts_with(&major_version)
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
    // `preview_connectors` → `connectors`) are already in place. Unlike the
    // major-version-only YAML migrations, this is a within-2.x rename, so it
    // must also run in `UpgradeMode::Minor` (the startup validation path) —
    // not just `UpgradeMode::Major` (the `router config upgrade` CLI).
    let (migrated_connectors_subgraphs, subgraphs_with_unpropagated_config) =
        migrate_connectors_subgraphs_to_sources(&mut config);
    if migrated_connectors_subgraphs {
        effective_descriptions.push(
            "Apollo Connectors `connectors.subgraphs` configuration has been replaced by `connectors.sources` keyed by `<subgraph>.<source>`".to_string(),
        );
    }
    if !subgraphs_with_unpropagated_config.is_empty() {
        effective_descriptions.push(format!(
            "Apollo Connectors: `connectors.subgraphs.<subgraph>.$config` was only copied onto sources listed under that subgraph's `sources` map. If your schema declares additional `@source`s for {}, add `$config` to their `connectors.sources` entries manually",
            subgraphs_with_unpropagated_config
                .iter()
                .map(|s| format!("`{s}`"))
                .join(", ")
        ));
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
/// `connectors.sources["<sub>.<src>"]`. Returns whether at least one field
/// was actually migrated, plus the names of subgraphs whose `$config` may
/// not have been fully propagated (see below) so the caller can warn.
///
/// Fields are merged in the same order the removed `apply_config` applied
/// them: when a `connectors.sources` entry already exists for a composite
/// key, the deprecated `override_url` / `max_requests_per_operation` values
/// win over it when present (matching the old per-field `if let Some(..)`
/// overwrites), and the subgraph-level `$config` always overwrites any
/// source-level `$config` — including on an existing entry — because the
/// deprecated branch ran last and unconditionally re-assigned
/// `connector.config`.
///
/// The deprecated runtime applied a subgraph's `$config` to *every*
/// connector under that subgraph, including ones for `@source`s not listed
/// in `connectors.subgraphs.<sub>.sources` (or with no `@source` at all).
/// This migration only sees the sources the deprecated config itself
/// listed (or none, if the subgraph declares `$config` without `sources`),
/// so it cannot fully replicate that behavior — any subgraph with a
/// `$config` is returned so the caller can tell the user to check for other
/// sources under that subgraph.
fn migrate_connectors_subgraphs_to_sources(config: &mut Value) -> (bool, Vec<String>) {
    let mut subgraphs_with_config = Vec::new();
    let Some(connectors) = config.get_mut("connectors").and_then(Value::as_object_mut) else {
        return (false, subgraphs_with_config);
    };
    let Some(Value::Object(subgraphs)) = connectors.remove("subgraphs") else {
        return (false, subgraphs_with_config);
    };

    let sources_entry = connectors
        .entry("sources".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    let Value::Object(sources) = sources_entry else {
        // `sources` is present but not an object — leave it alone and put
        // `subgraphs` back so the validation error surfaces the actual problem.
        connectors.insert("subgraphs".to_string(), Value::Object(subgraphs));
        return (false, subgraphs_with_config);
    };

    let mut migrated_any = false;
    for (subgraph_name, subgraph_value) in subgraphs {
        let Value::Object(mut subgraph_obj) = subgraph_value else {
            continue;
        };
        let parent_config = subgraph_obj.remove("$config");
        if parent_config.is_some() {
            subgraphs_with_config.push(subgraph_name.clone());
        }
        let Some(Value::Object(subgraph_sources)) = subgraph_obj.remove("sources") else {
            continue;
        };
        for (source_name, source_value) in subgraph_sources {
            let composite_key = format!("{subgraph_name}.{source_name}");
            let Value::Object(source_obj) = source_value else {
                // Non-object entries can't be merged field-by-field, so only
                // apply them when there's nothing to clobber.
                if !sources.contains_key(&composite_key) {
                    sources.insert(composite_key, source_value);
                    migrated_any = true;
                }
                continue;
            };

            if let Some(Value::Object(existing_obj)) = sources.get_mut(&composite_key) {
                let mut changed = false;
                for field in ["override_url", "max_requests_per_operation"] {
                    if let Some(value) = source_obj.get(field).filter(|v| !v.is_null()) {
                        existing_obj.insert(field.to_string(), value.clone());
                        changed = true;
                    }
                }
                if let Some(parent_config) = &parent_config {
                    existing_obj.insert("$config".to_string(), parent_config.clone());
                    changed = true;
                }
                migrated_any |= changed;
                continue;
            }

            let mut source_obj = source_obj;
            if let Some(parent_config) = &parent_config {
                source_obj.insert("$config".to_string(), parent_config.clone());
            }
            sources.insert(composite_key, Value::Object(source_obj));
            migrated_any = true;
        }
    }

    (migrated_any, subgraphs_with_config)
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
                // Select the value at `path` and compare it here, rather than letting JSONPath do
                // the comparison with a `$[?(@.a.b == x)]` filter. That filter form silently
                // matches nothing once `path` is more than two segments deep, which made the
                // whole action a no-op — no error, no warning, config left untouched.
                if jsonpath_lib::select(config, &format!("$.{path}"))
                    .unwrap_or_default()
                    .contains(&from)
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
        let (migrated, subgraphs_with_config) =
            migrate_connectors_subgraphs_to_sources(&mut config);
        assert!(migrated);
        assert_eq!(subgraphs_with_config, vec!["sub_a".to_string()]);
        insta::assert_json_snapshot!(config);
    }

    #[test]
    fn connectors_subgraphs_merges_with_existing_sources_deprecated_fields_win() {
        // Matches the deprecated runtime: the two shapes were applied
        // sequentially with the deprecated block running second, so its
        // per-field values (when present) overwrite the new-shape entry
        // rather than being dropped on the floor. Fields the deprecated
        // shape didn't set (e.g. `src_2` has no `max_requests_per_operation`
        // in either shape) are left untouched.
        let mut config = json!({
            "connectors": {
                "sources": {
                    "sub_a.src_1": { "override_url": "http://new", "max_requests_per_operation": 3 },
                    "sub_a.src_2": { "max_requests_per_operation": 7 }
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
        let (migrated, subgraphs_with_config) =
            migrate_connectors_subgraphs_to_sources(&mut config);
        assert!(migrated);
        assert!(subgraphs_with_config.is_empty());
        insta::assert_json_snapshot!(config);
    }

    #[test]
    fn connectors_subgraphs_no_op_when_absent() {
        let mut config = json!({
            "connectors": {
                "sources": { "sub_a.src_1": { "override_url": "http://one" } }
            }
        });
        let (migrated, subgraphs_with_config) =
            migrate_connectors_subgraphs_to_sources(&mut config);
        assert!(!migrated);
        assert!(subgraphs_with_config.is_empty());
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
        let (migrated, subgraphs_with_config) =
            migrate_connectors_subgraphs_to_sources(&mut config);
        assert!(migrated);
        assert_eq!(subgraphs_with_config, vec!["sub_a".to_string()]);
        insta::assert_json_snapshot!(config);
    }

    #[test]
    fn connectors_subgraphs_subgraph_config_overwrites_existing_source_config() {
        // Same precedence rule as above, but the source-level `$config`
        // lives on a pre-existing new-shape entry rather than a deprecated
        // per-source block — the subgraph-level value must still win, even
        // though the composite key already existed in `sources`.
        let mut config = json!({
            "connectors": {
                "sources": {
                    "sub_a.src_1": { "$config": { "key": "from-source" } }
                },
                "subgraphs": {
                    "sub_a": {
                        "$config": { "key": "from-subgraph" },
                        "sources": {
                            "src_1": {}
                        }
                    }
                }
            }
        });
        let (migrated, subgraphs_with_config) =
            migrate_connectors_subgraphs_to_sources(&mut config);
        assert!(migrated);
        assert_eq!(subgraphs_with_config, vec!["sub_a".to_string()]);
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
        let (migrated, subgraphs_with_config) =
            migrate_connectors_subgraphs_to_sources(&mut config);
        assert!(!migrated);
        assert!(subgraphs_with_config.is_empty());
        // empty `subgraphs:` is stripped so validation can proceed
        assert!(config["connectors"].get("subgraphs").is_none());
    }

    #[test]
    fn connectors_subgraphs_subgraph_without_sources_is_no_op_but_reports_config() {
        // A subgraph entry that only has $config (no .sources map) cannot be
        // expressed in the new shape — there's no source key to attach the
        // $config to. The function returns false (nothing was migrated) but
        // still reports the subgraph so the caller can warn that its
        // `$config` was dropped entirely.
        let mut config = json!({
            "connectors": {
                "subgraphs": {
                    "sub_a": { "$config": { "key": "lost" } }
                }
            }
        });
        let (migrated, subgraphs_with_config) =
            migrate_connectors_subgraphs_to_sources(&mut config);
        assert!(!migrated);
        assert_eq!(subgraphs_with_config, vec!["sub_a".to_string()]);
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

    /// Rewrites `"Error"` to `"error"` on a four-segment path. These assertions are deliberately
    /// explicit rather than insta snapshots, so a regression can't be blessed away by accepting a
    /// new snapshot.
    fn change_jwt_on_error(source: Value) -> Value {
        apply_migration(
            &source,
            &Migration::builder()
                .action(Action::Change {
                    path: "authentication.router.jwt.on_error".to_string(),
                    from: Value::String("Error".into()),
                    to: Value::String("error".into()),
                })
                .description("rename a deeply nested value")
                .build(),
        )
        .expect("expected successful migration")
    }

    /// `Change` used to compare the current value with a `$[?(@.a.b == x)]` filter, which matches
    /// nothing once the path is more than two segments deep. Any migration rewriting a value on a
    /// deeply nested path was silently a no-op — this asserts it isn't.
    #[test]
    fn change_field_deeply_nested() {
        assert_eq!(
            change_jwt_on_error(
                json!({"authentication": {"router": {"jwt": {"on_error": "Error"}}}})
            ),
            json!({"authentication": {"router": {"jwt": {"on_error": "error"}}}})
        );
    }

    /// Guards the other direction: `from` must still be honoured at depth, so a non-matching value
    /// is left alone rather than rewritten unconditionally.
    #[test]
    fn change_field_deeply_nested_only_matching_value() {
        let source = json!({"authentication": {"router": {"jwt": {"on_error": "Continue"}}}});
        assert_eq!(change_jwt_on_error(source.clone()), source);
    }

    /// A `Change` on a path that isn't present must not create it.
    #[test]
    fn change_field_deeply_nested_absent_path() {
        let source = json!({"authentication": {"router": {"jwt": {}}}});
        assert_eq!(change_jwt_on_error(source.clone()), source);
    }
}
