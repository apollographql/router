//! Compares effective settings from router's loader and `apollo-configuration`.
//! Named cases record differences in diagnostics, expansion and configuration-usage data.

use std::collections::HashMap;
use std::str::FromStr;

use apollo_configuration::ParseYamlOptions;
use apollo_configuration::expansion::FileVariables;
use apollo_configuration::expansion::MapVariables;
use apollo_configuration::provenance::Injection;
use serde_json::Value;
use serde_json::json;

use super::Configuration;
use super::expansion::Expansion;
use super::expansion::Override;
use super::expansion::ValueType;
use super::schema::Mode;
use super::schema::generate_config_schema;
use super::schema::validate_yaml_configuration;
use super::test_discovery;
use super::upgrade::UpgradeMode;
use super::upgrade::upgrade_configuration;
use crate::plugins::healthcheck::Config as HealthCheck;
use crate::plugins::subscription::SubscriptionConfig;

// Configuration's Deserialize implementation already runs its cross-field validation.
impl apollo_configuration::Validate for Configuration {}
impl apollo_configuration::Configuration for Configuration {}

fn current_major_version() -> i64 {
    env!("CARGO_PKG_VERSION_MAJOR")
        .parse()
        .expect("CARGO_PKG_VERSION_MAJOR should be an integer")
}

fn router_options() -> ParseYamlOptions {
    // Use the same schema, including router's additionalProperties patch, for both parsers.
    let schema = serde_json::to_value(generate_config_schema())
        .expect("router's configuration schema serializes");
    ParseYamlOptions::default().schema(schema)
}

/// Which migration, if any, a corpus fixture needs before the shared parser can accept it.
#[derive(Clone, Copy)]
enum Migration {
    /// Already in the current on-disk shape.
    None,
    /// Needs a migration whose filename is prefixed with the current major version.
    /// `validate_yaml_configuration(.., Mode::Upgrade)` applies these automatically, so router's
    /// own startup path accepts the fixture as-is.
    Minor,
    /// Needs a migration outside the current major version's prefix. Startup rejects this
    /// fixture outright; only `router config upgrade` (`UpgradeMode::Major`) can bring it
    /// forward, so both sides of the comparison run it through that step first.
    Major,
}

/// One corpus fixture and how to compare it. `name` identifies the case in a failing assertion,
/// independently of the file name.
struct Case {
    name: &'static str,
    text: &'static str,
    migration: Migration,
}

const FEATUREFUL_CASE: Case = Case {
    name: "cors policies, apq, persisted queries, batching, limits, health check, \
           subscription, a hidden built-in plugin and a custom plugin",
    text: include_str!("testdata/compat/current_featureful.yaml"),
    migration: Migration::None,
};

const CASES: &[Case] = &[
    Case {
        name: "minimal supergraph listener",
        text: include_str!("testdata/compat/current_minimal.yaml"),
        migration: Migration::None,
    },
    FEATUREFUL_CASE,
    Case {
        name: "batching integration configuration",
        text: include_str!("../../tests/fixtures/batching/all_enabled.router.yaml"),
        migration: Migration::None,
    },
    Case {
        name: "documented persisted-query safelist configuration",
        text: include_str!("../../../examples/persisted-queries/safelist_pq_require_id.yaml"),
        migration: Migration::None,
    },
    Case {
        name: "cors.origins migrates into cors.policies",
        text: include_str!("testdata/compat/needs_minor_migration_cors_origins.yaml"),
        migration: Migration::Minor,
    },
    Case {
        name: "flat headers.all.request migrates under an operations key",
        text: include_str!("testdata/compat/needs_minor_migration_headers_flat_list.yaml"),
        migration: Migration::Minor,
    },
    Case {
        name: "flat subscription.deduplication migrates under deduplication.all",
        text: include_str!("testdata/migrations/subscription_dedup_subgraph.yaml"),
        migration: Migration::Minor,
    },
    Case {
        name: "experimental_batching renamed to batching (breaking; needs `router config upgrade`)",
        text: include_str!("testdata/migrations/batching.yaml"),
        migration: Migration::Major,
    },
];

/// Applies `mode`'s migrations to `text` and reserializes the result, the way the loader does
/// before it reparses a migrated document.
fn migrate(text: &str, mode: UpgradeMode) -> Result<String, String> {
    let raw: Value = serde_yaml::from_str(text).map_err(|error| error.to_string())?;
    let migrated = upgrade_configuration(&raw, false, mode).map_err(|error| error.to_string())?;
    serde_yaml::to_string(&migrated).map_err(|error| error.to_string())
}

/// Parses `case` the way router's own startup path does: `Configuration::from_str`'s
/// `Mode::Upgrade` migrates within-major shapes automatically, and a case needing a major
/// migration is pre-upgraded first, standing in for an operator running `router config upgrade`
/// before startup would otherwise reject the file.
fn router_effective_settings(case: &Case) -> Result<Configuration, String> {
    let text = case.text;
    match case.migration {
        Migration::None | Migration::Minor => {
            validate_yaml_configuration(text, Expansion::builder().build(), Mode::Upgrade)
                .map_err(|error| error.to_string())
        }
        Migration::Major => {
            let upgraded_yaml = migrate(text, UpgradeMode::Major)?;
            validate_yaml_configuration(
                &upgraded_yaml,
                Expansion::builder().build(),
                Mode::NoUpgrade,
            )
            .map_err(|error| error.to_string())
        }
    }
}

/// Migrates legacy fixtures before passing them to the shared parser.
fn shared_effective_settings(case: &Case) -> Result<Configuration, String> {
    let text = case.text;
    let text = match case.migration {
        Migration::None => text.to_string(),
        Migration::Minor => migrate(text, UpgradeMode::Minor(current_major_version()))?,
        Migration::Major => migrate(text, UpgradeMode::Major)?,
    };
    router_options()
        .parse::<Configuration>(&text)
        .map_err(|error| format!("{:?}", miette::Report::new(error)))
}

/// The JSON pointer to the first leaf where `a` and `b` disagree, or `None` if they match.
fn first_difference(a: &Value, b: &Value) -> Option<String> {
    fn walk(a: &Value, b: &Value, path: &mut String) -> Option<String> {
        match (a, b) {
            (Value::Object(a_map), Value::Object(b_map)) => {
                let mut keys: Vec<&String> = a_map.keys().chain(b_map.keys()).collect();
                keys.sort();
                keys.dedup();
                for key in keys {
                    let prefix_len = path.len();
                    path.push('/');
                    path.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    let found = match (a_map.get(key), b_map.get(key)) {
                        (Some(av), Some(bv)) => walk(av, bv, path),
                        (None, None) => None,
                        _ => Some(path.clone()),
                    };
                    if found.is_some() {
                        return found;
                    }
                    path.truncate(prefix_len);
                }
                None
            }
            (Value::Array(a_items), Value::Array(b_items)) if a_items.len() == b_items.len() => {
                for (index, (av, bv)) in a_items.iter().zip(b_items).enumerate() {
                    let prefix_len = path.len();
                    path.push('/');
                    path.push_str(&index.to_string());
                    let found = walk(av, bv, path);
                    if found.is_some() {
                        return found;
                    }
                    path.truncate(prefix_len);
                }
                None
            }
            _ if a == b => None,
            _ => Some(path.clone()),
        }
    }

    let mut path = String::new();
    walk(a, b, &mut path)
}

#[test]
fn difference_paths_resolve_object_keys_and_array_entries() {
    for (left, right, expected) in [
        (
            json!({"a/b~c": [1, 2]}),
            json!({"a/b~c": [1, 3]}),
            "/a~1b~0c/1",
        ),
        (json!({"a": 1}), json!({}), "/a"),
        (json!({}), json!({"a": 1}), "/a"),
        (json!([1]), json!([1, 2]), ""),
        (json!(null), json!(false), ""),
    ] {
        let path = first_difference(&left, &right).expect("values differ");
        assert_eq!(path, expected);
        assert_ne!(left.pointer(&path), right.pointer(&path));
    }
    let identical = json!({"a": [null, true, 1, "text", {}]});
    assert_eq!(first_difference(&identical, &identical), None);
}

/// Current-format inputs, and inputs migrated ahead of time exactly as a real deployment would
/// migrate them, must produce the same effective settings through both parsers. A mismatch
/// names the case and the configuration path where the two disagree.
#[test]
fn effective_settings_agree_for_the_shared_corpus() {
    for case in CASES {
        let router = router_effective_settings(case).unwrap_or_else(|error| {
            panic!(
                "[{}] router's own pipeline rejected a fixture expected to succeed: {error}",
                case.name
            )
        });
        let shared = shared_effective_settings(case).unwrap_or_else(|error| {
            panic!(
                "[{}] the shared parser rejected a fixture expected to succeed: {error}",
                case.name
            )
        });
        let router_json = serde_json::to_value(&router).expect("Configuration serializes");
        let shared_json = serde_json::to_value(&shared).expect("Configuration serializes");
        if let Some(path) = first_difference(&router_json, &shared_json) {
            panic!(
                "[{}] router's own pipeline and the shared parser disagree at `{path}`\n\
                 router:  {}\n\
                 shared:  {}",
                case.name,
                router_json.pointer(&path).unwrap_or(&Value::Null),
                shared_json.pointer(&path).unwrap_or(&Value::Null),
            );
        }
    }
}

#[test]
fn schema_derived_boolean_values_agree_between_parsers() {
    let schema = serde_json::to_value(generate_config_schema()).unwrap();
    let default = schema["properties"]["experimental_type_conditioned_fetching"]["default"]
        .as_bool()
        .expect("the schema declares a boolean default");
    for enabled in [default, !default] {
        let text = format!("experimental_type_conditioned_fetching: {enabled}\n");
        let router =
            validate_yaml_configuration(&text, Expansion::builder().build(), Mode::NoUpgrade)
                .expect("schema-derived input is valid");
        let shared = router_options()
            .parse::<Configuration>(&text)
            .expect("schema-derived input is valid");
        assert_eq!(router.experimental_type_conditioned_fetching, enabled);
        assert_eq!(shared.experimental_type_conditioned_fetching, enabled);
    }
}

/// An unknown top-level key is a schema violation on both sides, because the shared parser uses
/// router's own patched root schema (see `router_options`).
#[test]
fn unknown_top_level_key_is_rejected_by_both_parsers() {
    let text = include_str!("testdata/compat/unknown_top_level_key.yaml");
    validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
        .expect_err("router's own pipeline should reject the unknown key");
    router_options()
        .parse::<Configuration>(text)
        .expect_err("the shared parser should reject the unknown key too");
}

/// An unregistered plugin name is rejected the same way: `UserPlugins`'s generated schema sets
/// `additionalProperties: false`, so a name outside the registry fails validation before router's
/// own runtime unknown-plugin check would even run.
#[test]
fn unknown_plugin_name_is_rejected_by_both_parsers() {
    let text = include_str!("testdata/compat/unknown_plugin_name.yaml");
    validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
        .expect_err("router's own pipeline should reject the unregistered plugin name");
    router_options()
        .parse::<Configuration>(text)
        .expect_err("the shared parser should reject it too");
}

/// Router normalizes `plugins: null` to an empty map before schema validation.
/// The shared parser validates the null value and rejects it.
#[test]
fn null_plugins_requires_router_normalization() {
    let text = "plugins: null\n";
    let router = validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
        .expect("router accepts null plugin settings");
    assert!(
        router
            .plugins
            .plugins
            .as_ref()
            .is_none_or(|plugins| plugins.is_empty())
    );
    let error = router_options()
        .parse::<Configuration>(text)
        .expect_err("the shared parser requires an object for plugins");
    let messages = miette::Diagnostic::related(&error)
        .into_iter()
        .flatten()
        .map(|diagnostic| diagnostic.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(messages.contains("is not of type \"object\""), "{messages}");
}

/// Duplicate-key diagnostics differ: router reports "duplicated keys", while the shared
/// parser reports serde_yaml's "duplicate entry" error.
#[test]
fn duplicate_top_level_keys_is_rejected_by_both_with_different_messages() {
    let text = include_str!("testdata/compat/duplicate_keys.yaml");

    let router_error =
        validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
            .expect_err("router's own pipeline rejects duplicate keys before validation");
    assert!(
        router_error.to_string().contains("duplicated keys"),
        "expected a duplicated-keys error, got: {router_error}"
    );

    let shared_error = router_options()
        .parse::<Configuration>(text)
        .expect_err("the shared parser also rejects the duplicated key, via serde_yaml itself");
    assert!(
        format!("{shared_error}").contains("duplicate entry"),
        "expected serde_yaml's own duplicate-entry message, got: {shared_error}"
    );
}

/// Configuration-usage selectors read `validated_yaml`. The shared parser leaves it empty;
/// `parse_via_apollo_configuration` supplies the expanded document for those selectors.
#[test]
fn configuration_usage_telemetry_needs_the_adapter_to_populate_validated_yaml() {
    let text = "persisted_queries:\n  enabled: true\n";

    let bare_shared: Configuration = router_options()
        .parse(text)
        .expect("the shared parser accepts this");
    assert!(
        bare_shared.validated_yaml.is_none(),
        "a bare shared-parser call has nothing to populate `validated_yaml` from"
    );

    let router_config =
        validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
            .expect("router's own pipeline accepts this");
    let adapted_shared =
        parse_via_apollo_configuration(text, &router_options(), &Expansion::builder().build())
            .expect("the adapter accepts this");

    let selector =
        jsonpath_rust::JsonPathInst::from_str("$.persisted_queries[?(@.enabled == true)]")
            .expect("valid path");
    let router_hits = selector
        .find_slice(
            router_config
                .validated_yaml
                .as_ref()
                .expect("router sets this"),
        )
        .len();
    let adapted_hits = selector
        .find_slice(
            adapted_shared
                .validated_yaml
                .as_ref()
                .expect("the adapter sets this"),
        )
        .len();
    assert_eq!(
        router_hits, adapted_hits,
        "the usage gauge for `apollo.router.config.persisted_queries` must see the same match count"
    );
    assert_eq!(
        router_hits, 1,
        "the fixture enables persisted queries, so the gauge must fire"
    );
}

/// Parses settings and supplies `validated_yaml` for configuration-usage selectors.
/// Configure `options` and `expansion` with equivalent external inputs.
fn parse_via_apollo_configuration(
    text: &str,
    options: &ParseYamlOptions,
    expansion: &Expansion,
) -> Result<Configuration, String> {
    let raw: Value = serde_yaml::from_str(text).map_err(|error| error.to_string())?;
    let expanded = expansion.expand(&raw).map_err(|error| error.to_string())?;
    let mut config: Configuration = options
        .parse(text)
        .map_err(|error| format!("{:?}", miette::Report::new(error)))?;
    config.validated_yaml = Some(expanded);
    Ok(config)
}

#[test]
fn invalid_migrated_input_is_rejected_by_both_parsers() {
    let case = Case {
        name: "unknown key after a minor migration",
        text: "cors:\n  origins:\n    - \"https://example.com\"\nthis_key_does_not_exist_anywhere: true\n",
        migration: Migration::Minor,
    };
    for error in [
        router_effective_settings(&case).expect_err("router rejects the migrated input"),
        shared_effective_settings(&case).expect_err("the shared parser rejects the migrated input"),
    ] {
        assert!(
            error.contains("this_key_does_not_exist_anywhere"),
            "{error}"
        );
    }
}

#[test]
fn introspection_defaults_and_explicit_values_agree_between_parsers() {
    let previous_text = include_str!("testdata/compat/current_minimal.yaml");
    let next_text = include_str!("testdata/compat/current_minimal_v2.yaml");

    let router_previous =
        validate_yaml_configuration(previous_text, Expansion::builder().build(), Mode::NoUpgrade)
            .expect("router's own pipeline accepts the previous configuration");
    let shared_previous = router_options()
        .parse::<Configuration>(previous_text)
        .expect("the shared parser accepts the previous configuration");

    let router_next =
        validate_yaml_configuration(next_text, Expansion::builder().build(), Mode::NoUpgrade)
            .expect("router's own pipeline accepts the replacement configuration");
    let shared_next = router_options()
        .parse::<Configuration>(next_text)
        .expect("the shared parser accepts the replacement configuration");

    assert!(!router_previous.supergraph.introspection);
    assert!(!shared_previous.supergraph.introspection);
    assert!(router_next.supergraph.introspection);
    assert!(shared_next.supergraph.introspection);
}

struct TypedApolloPlugins {
    health_check: HealthCheck,
    subscription: SubscriptionConfig,
}

fn typed_apollo_plugins(config: &Configuration) -> Result<TypedApolloPlugins, String> {
    fn raw(config: &Configuration, name: &str) -> Result<Value, String> {
        config
            .apollo_plugins
            .plugins
            .get(name)
            .cloned()
            .ok_or_else(|| format!("the parsed configuration holds no `{name}` plugin config"))
    }

    Ok(TypedApolloPlugins {
        health_check: serde_json::from_value(raw(config, "health_check")?)
            .map_err(|error| error.to_string())?,
        subscription: serde_json::from_value(raw(config, "subscription")?)
            .map_err(|error| error.to_string())?,
    })
}

/// Mandatory plugin defaults (`limits`, `health_check`) are present even when the document never
/// mentions them, on both sides, because both call the same `Configuration::deserialize`.
#[test]
fn mandatory_plugin_defaults_are_present_without_being_configured() {
    let text = include_str!("testdata/compat/current_minimal.yaml");
    let router = validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
        .expect("router's own pipeline accepts this");
    let shared = router_options()
        .parse::<Configuration>(text)
        .expect("the shared parser accepts this");

    for plugin in ["limits", "health_check"] {
        assert!(
            router.apollo_plugins.plugins.contains_key(plugin),
            "router's own pipeline must default the mandatory `{plugin}` plugin entry"
        );
        assert!(
            shared.apollo_plugins.plugins.contains_key(plugin),
            "the shared parser must default the mandatory `{plugin}` plugin entry too"
        );
    }
}

/// Both parsers retain raw plugin settings. Deserialize those settings into the plugin types
/// to check subscription deduplication and health-check defaults beyond schema validation.
#[tokio::test]
async fn typed_plugin_configs_and_initialization_agree() {
    let case = &FEATUREFUL_CASE;
    let router = router_effective_settings(case).expect("the featureful fixture is valid");
    let shared = shared_effective_settings(case).expect("the featureful fixture is valid");

    let router_typed =
        typed_apollo_plugins(&router).expect("router's raw plugin config is typed-deserializable");
    let shared_typed = typed_apollo_plugins(&shared)
        .expect("the shared parser's raw plugin config is typed-deserializable");

    assert_eq!(
        serde_json::to_value(&router_typed.subscription).unwrap(),
        serde_json::to_value(&shared_typed.subscription).unwrap(),
        "typed subscription config, including deduplication, must agree"
    );
    assert_eq!(
        serde_json::to_value(&router_typed.health_check).unwrap(),
        serde_json::to_value(&shared_typed.health_check).unwrap(),
        "typed health_check config must agree"
    );

    for name in ["health_check", "subscription"] {
        let factory = crate::plugin::plugins()
            .find(|factory| factory.name == format!("apollo.{name}"))
            .expect("built-in plugin is registered");
        for config in [&router, &shared] {
            factory
                .create_instance(
                    crate::plugin::PluginInit::fake_builder()
                        .config(config.apollo_plugins.plugins[name].clone())
                        .build(),
                )
                .await
                .unwrap_or_else(|error| panic!("{name} initialization failed: {error}"));
        }
    }
}

/// The flat deduplication shape passes schema validation but fails typed deserialization.
/// Migration must nest its settings under `deduplication.all` before plugin initialization.
#[test]
fn unmigrated_flat_subscription_dedup_fails_at_plugin_init_not_at_parse() {
    let text = include_str!("testdata/migrations/subscription_dedup_subgraph.yaml");

    let shared = router_options()
        .parse::<Configuration>(text)
        .expect("Configuration-level parsing accepts the unmigrated flat shape");
    let raw_subscription = shared
        .apollo_plugins
        .plugins
        .get("subscription")
        .cloned()
        .expect("subscription config is present");
    let plugin_init_result: Result<SubscriptionConfig, _> =
        serde_json::from_value(raw_subscription);
    assert!(
        plugin_init_result.is_err(),
        "the unmigrated flat shape must fail SubscriptionConfig's typed deserialize at plugin \
         construction, the way it does through router's own pipeline today"
    );
}

/// A configuration passing router's JSON Schema but failing router's own custom validation
/// (`Configuration::validate`, called from inside `Configuration::deserialize`) is rejected
/// identically by both parsers, because both call the very same `Deserialize` impl.
#[test]
fn cross_field_validation_embedded_in_deserialize_rejects_both_the_same_way() {
    let text = "sandbox:\n  enabled: true\nhomepage:\n  enabled: true\nsupergraph:\n  introspection: true\n";
    let router_error =
        validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
            .expect_err("router's own pipeline rejects sandbox and homepage both enabled");
    let shared_error = router_options()
        .parse::<Configuration>(text)
        .expect_err("the shared parser rejects it too, via the same Deserialize impl");
    assert!(
        router_error
            .to_string()
            .contains("sandbox and homepage cannot be enabled")
    );
    assert!(
        shared_error
            .to_string()
            .contains("sandbox and homepage cannot be enabled")
    );
}

/// Exercises the shared crate's custom-validation hook with a synthetic configuration type.
mod custom_plugin_validation {
    use apollo_configuration::ErrorCollector;
    use apollo_configuration::ParseYamlOptions;
    use apollo_configuration::configuration;
    use miette::Diagnostic as _;

    // Zero passes the integer schema but fails the custom validator.
    #[configuration(validate = validate_widget)]
    struct WidgetConfig {
        #[config(default = 1)]
        #[allow(dead_code)]
        replica_count: u32,
        #[config(default = 1)]
        #[allow(dead_code)]
        shard_count: u32,
    }

    #[configuration]
    struct PluginConfig {
        widget: WidgetConfig,
    }

    fn validate_widget(config: &WidgetConfig, mut errors: ErrorCollector<'_>) {
        if config.replica_count == 0 {
            errors
                .nest("replica_count")
                .report_simple("replica_count must be at least 1");
        }
        if config.shard_count == 0 {
            errors
                .nest("shard_count")
                .report_simple("shard_count must be at least 1");
        }
    }

    /// Passes JSON Schema (both fields are integers) but fails the custom validator, and the
    /// shared parse call (`ParseYamlOptions::parse`) is what surfaces the rejection.
    #[test]
    fn schema_valid_input_is_rejected_by_the_custom_validator() {
        let yaml = "replica_count: 0\nshard_count: 1\n";
        let error = ParseYamlOptions::default()
            .parse::<WidgetConfig>(yaml)
            .expect_err("zero replicas passes the schema but fails custom validation");
        let messages: Vec<_> = error.related().unwrap().map(ToString::to_string).collect();
        assert_eq!(messages, ["replica_count must be at least 1"]);

        ParseYamlOptions::default()
            .parse::<WidgetConfig>("replica_count: 1\nshard_count: 1\n")
            .expect("positive counts pass custom validation");
    }

    /// Multiple custom-validation errors on nested fields are all reported, each labeled at its
    /// own location in the source document rather than collapsed into one error at the root.
    #[test]
    fn multiple_custom_validation_errors_keep_distinct_nested_locations() {
        let yaml = "widget:\n  replica_count: 0\n  shard_count: 0\n";
        let error = ParseYamlOptions::default()
            .parse::<PluginConfig>(yaml)
            .expect_err("both fields are invalid");

        let related: Vec<_> = error
            .related()
            .expect("two independent validation errors were reported")
            .collect();
        assert_eq!(
            related.len(),
            2,
            "one error per invalid field, not one merged error"
        );

        for (field, offset) in [
            ("replica_count", yaml.find('0').unwrap()),
            ("shard_count", yaml.rfind('0').unwrap()),
        ] {
            let message = format!("{field} must be at least 1");
            let diagnostic = related
                .iter()
                .find(|diagnostic| diagnostic.to_string() == message)
                .expect("each field has its own validation message");
            let spans: Vec<_> = diagnostic.labels().unwrap().collect();
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].offset(), offset, "{field}");
            assert_eq!(spans[0].len(), 1, "{field}");
        }
    }
}

#[test]
fn env_expansion_agrees_between_the_two_expanders() {
    let text = "supergraph:\n  listen: 127.0.0.1:${env.COMPAT_TEST_PORT}\n";

    let router_expansion = Expansion::builder()
        .supported_mode("env")
        .mocked_env_var("COMPAT_TEST_PORT", "4001")
        .build();
    let router_config = validate_yaml_configuration(text, router_expansion, Mode::NoUpgrade)
        .expect("router's own expansion resolves this");

    let shared_options = router_options().add_variables(MapVariables(HashMap::from([(
        "COMPAT_TEST_PORT".to_string(),
        "4001".to_string(),
    )])));
    let shared_config = shared_options
        .parse::<Configuration>(text)
        .expect("the shared crate's expansion resolves this too");

    assert_eq!(
        router_config.supergraph.listen,
        shared_config.supergraph.listen
    );
}

/// Router's env-var overrides (`Expansion::Override`) write a value at a fixed config path
/// regardless of whether the document contains a `${...}` placeholder there at all -- unlike
/// expansion, which only replaces a placeholder that is already written. `apollo_configuration`'s
/// `provenance::Injection` is the equivalent: both take precedence over whatever the document
/// itself set at that path.
#[test]
fn an_env_var_override_beats_the_documents_own_value_on_both_sides() {
    let text = "supergraph:\n  listen: 127.0.0.1:4000\n";

    let router_override = Override::builder()
        .config_path("supergraph.listen")
        .env_name("COMPAT_TEST_LISTEN_OVERRIDE")
        .value_type(ValueType::String)
        .mocked_env_var("COMPAT_TEST_LISTEN_OVERRIDE", "127.0.0.1:9999")
        .build();
    let router_expansion = Expansion::builder()
        .override_config(router_override)
        .build();
    let router_config = validate_yaml_configuration(text, router_expansion, Mode::NoUpgrade)
        .expect("router's own override mechanism applies this");

    let shared_options = router_options().inject(vec![Injection::env(
        &["supergraph", "listen"],
        json!("127.0.0.1:9999"),
        "COMPAT_TEST_LISTEN_OVERRIDE",
    )]);
    let shared_config = shared_options
        .parse::<Configuration>(text)
        .expect("the shared crate's injection mechanism applies this too");

    assert_eq!(
        router_config.supergraph.listen.to_string(),
        "http://127.0.0.1:9999"
    );
    assert_eq!(
        shared_config.supergraph.listen.to_string(),
        "http://127.0.0.1:9999"
    );
}

#[test]
fn file_expansion_agrees_between_the_two_expanders_for_a_root_level_field() {
    let mut file = tempfile::NamedTempFile::new().expect("can create a temp file");
    std::io::Write::write_all(&mut file, b"true").expect("can write the temp file");
    let text = format!(
        "experimental_type_conditioned_fetching: ${{file.{}}}\n",
        file.path().to_string_lossy()
    );

    let router_expansion = Expansion::builder().supported_mode("file").build();
    let router_config = validate_yaml_configuration(&text, router_expansion, Mode::NoUpgrade)
        .expect("router's own file expansion resolves this");

    let shared_options = router_options().add_variables(FileVariables);
    let shared_config = shared_options
        .parse::<Configuration>(&text)
        .expect("the shared crate's file expansion resolves this too");

    assert!(router_config.experimental_type_conditioned_fetching);
    assert!(shared_config.experimental_type_conditioned_fetching);
}

/// The same expansion one level below the document root. Coercion has to resolve the field's
/// declared type through the `allOf` wrapper schemars emits around a nested struct's reference,
/// which is what `apollo-configuration` 0.6.2 added (PLAT-303). Before it, the shared parser
/// left the value a string and failed the schema's boolean check.
#[test]
fn file_expansion_boolean_coercion_resolves_through_a_nested_allof_ref() {
    let mut file = tempfile::NamedTempFile::new().expect("can create a temp file");
    std::io::Write::write_all(&mut file, b"true").expect("can write the temp file");
    let text = format!(
        "supergraph:\n  introspection: ${{file.{}}}\n",
        file.path().to_string_lossy()
    );

    let router_expansion = Expansion::builder().supported_mode("file").build();
    let router_config = validate_yaml_configuration(&text, router_expansion, Mode::NoUpgrade)
        .expect("router's own file expansion resolves this");

    let shared_options = router_options().add_variables(FileVariables);
    let shared_config = shared_options
        .parse::<Configuration>(&text)
        .expect("the shared crate resolves the nested field's declared type too");

    assert!(router_config.supergraph.introspection);
    assert!(shared_config.supergraph.introspection);
}

#[test]
fn telemetry_input_agrees_between_the_two_parsers() {
    let text = include_str!("testdata/tracing_config.router.yaml");
    let router = validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
        .expect("router's own pipeline accepts this telemetry input");
    let shared = router_options()
        .parse::<Configuration>(text)
        .expect("the shared parser accepts this telemetry input too");

    let router_json = serde_json::to_value(&router).expect("Configuration serializes");
    let shared_json = serde_json::to_value(&shared).expect("Configuration serializes");
    if let Some(path) = first_difference(&router_json, &shared_json) {
        panic!("telemetry input: router's own pipeline and the shared parser disagree at `{path}`");
    }
}

/// Runs every integration fixture, published example, and docs `router.yaml` snippet that
/// [`test_discovery::discover_project_configs`] finds through both parsers, with the same mocked
/// environment variables on each side, and compares effective settings the way
/// `effective_settings_agree_for_the_shared_corpus` compares `CASES`.
///
/// Any rejection or disagreement fails the test naming the document's path.
#[test]
fn effective_settings_agree_for_discovered_project_documents() {
    let mocked_env_vars = test_discovery::discovery_env_vars();
    let mut compared = 0usize;
    let mut unexpected = Vec::new();

    for doc in test_discovery::discover_project_configs() {
        let router_expansion = Expansion::default_builder()
            .mocked_env_vars(mocked_env_vars.clone())
            .build()
            .unwrap();
        let router = match validate_yaml_configuration(&doc.yaml, router_expansion, Mode::NoUpgrade)
        {
            Ok(config) => config,
            Err(error) => {
                unexpected.push(format!(
                    "{}: router's own pipeline rejected a discovered document expected to \
                     succeed: {error}",
                    doc.path.display()
                ));
                continue;
            }
        };

        let shared_options = router_options().add_variables(MapVariables(mocked_env_vars.clone()));
        match shared_options.parse::<Configuration>(&doc.yaml) {
            Ok(shared) => {
                compared += 1;
                let router_json = serde_json::to_value(&router).expect("Configuration serializes");
                let shared_json = serde_json::to_value(&shared).expect("Configuration serializes");
                if let Some(path) = first_difference(&router_json, &shared_json) {
                    unexpected.push(format!(
                        "{}: router's own pipeline and the shared parser disagree at `{path}`",
                        doc.path.display()
                    ));
                }
            }
            Err(error) => unexpected.push(format!(
                "{}: the shared parser rejected a discovered document expected to succeed: {:?}",
                doc.path.display(),
                miette::Report::new(error)
            )),
        }
    }

    assert!(
        unexpected.is_empty(),
        "discovered documents disagree between router's own pipeline and the shared parser:\n\n{}",
        unexpected.join("\n\n")
    );
    assert!(
        compared > 0,
        "expected to discover at least one project configuration document"
    );
}

/// Extends `CASES`' curated coverage with every top-level property whose generated schema
/// declares a scalar `default`: a document setting the property to that exact value must produce
/// the same effective settings on both sides, the way
/// `schema_derived_boolean_values_agree_between_parsers` already checks for one such property.
#[test]
fn schema_declared_top_level_defaults_agree_between_parsers() {
    let schema = serde_json::to_value(generate_config_schema()).expect("schema serializes");
    let properties = schema["properties"]
        .as_object()
        .expect("the root schema declares properties");

    let mut checked = 0usize;
    for (key, property_schema) in properties {
        let Some(default) = property_schema.get("default") else {
            continue;
        };
        if !(default.is_boolean() || default.is_string() || default.is_number()) {
            continue;
        }
        checked += 1;

        // A JSON scalar is valid YAML flow-scalar syntax, so `default`'s `Display` (JSON text)
        // can be written directly after the key without a round trip through `serde_yaml`.
        let text = format!("{key}: {default}\n");

        let router = validate_yaml_configuration(
            &text,
            Expansion::builder().build(),
            Mode::NoUpgrade,
        )
        .unwrap_or_else(|error| {
            panic!(
                "[{key}] router's own pipeline rejected its own schema-declared default: {error}"
            )
        });
        let shared = router_options()
            .parse::<Configuration>(&text)
            .unwrap_or_else(|error| {
                panic!("[{key}] the shared parser rejected the schema-declared default: {error}")
            });

        let router_json = serde_json::to_value(&router).expect("Configuration serializes");
        let shared_json = serde_json::to_value(&shared).expect("Configuration serializes");
        if let Some(path) = first_difference(&router_json, &shared_json) {
            panic!(
                "[{key}] router's own pipeline and the shared parser disagree at `{path}` when \
                 the document sets the schema's own declared default"
            );
        }
    }

    assert!(
        checked > 0,
        "expected at least one top-level property with a scalar schema default"
    );
}

/// `RateLimitConf` (`traffic_shaping.{router,all}.global_rate_limit`) is one of the schema's
/// genuinely required-field structs. Most of `Configuration`'s nested structs carry a
/// struct-level default that keeps their fields out of the schema's `required` array even when
/// the fields themselves have no default. `RateLimitConf` has no such struct-level default, so
/// schemars marks `capacity` and `interval` required. Two different valid value permutations must
/// parse to the same effective settings on both sides.
#[test]
fn schema_derived_required_field_permutations_agree_between_parsers() {
    for (capacity, interval) in [(10, "1s"), (500, "30s")] {
        let text = format!(
            "traffic_shaping:\n  all:\n    global_rate_limit:\n      capacity: {capacity}\n      interval: {interval}\n"
        );

        let router =
            validate_yaml_configuration(&text, Expansion::builder().build(), Mode::NoUpgrade)
                .unwrap_or_else(|error| {
                    panic!(
                        "[capacity={capacity}, interval={interval}] router's own pipeline rejected \
                     a document supplying RateLimitConf's required fields: {error}"
                    )
                });
        let shared = router_options()
            .parse::<Configuration>(&text)
            .unwrap_or_else(|error| {
                panic!(
                    "[capacity={capacity}, interval={interval}] the shared parser rejected a \
                     document supplying RateLimitConf's required fields: {error}"
                )
            });

        let router_json = serde_json::to_value(&router).expect("Configuration serializes");
        let shared_json = serde_json::to_value(&shared).expect("Configuration serializes");
        if let Some(path) = first_difference(&router_json, &shared_json) {
            panic!(
                "[capacity={capacity}, interval={interval}] router's own pipeline and the shared \
                 parser disagree at `{path}`"
            );
        }
    }
}

/// Licence gating (`ConfigurationRestriction` matching in
/// `uplink::license_enforcement::LicenseEnforcementReport::configuration_restrictions`) runs its
/// JSONPath selectors over an already-parsed `Configuration`'s `validated_yaml` field, not over
/// parsing itself. That field is `#[serde(skip)]`, so it never appears in the `first_difference`
/// comparisons the rest of this corpus relies on.
///
/// Agreement here rests on a mechanism this corpus already proves for one selector:
/// `configuration_usage_telemetry_needs_the_adapter_to_populate_validated_yaml`. This case
/// exercises the same mechanism against representative restriction paths read directly from
/// `license_enforcement.rs` at the time of writing: `$.batching` and `$.persisted_queries` are
/// bare presence checks, and `$.subscription.enabled` is a presence-plus-value check. It adds no
/// new machinery for licence enforcement itself.
#[test]
fn licence_restricted_configuration_paths_agree_between_parsers_via_validated_yaml() {
    let text = "batching:\n  enabled: true\npersisted_queries:\n  enabled: true\nsubscription:\n  enabled: true\n";

    let router = validate_yaml_configuration(text, Expansion::builder().build(), Mode::NoUpgrade)
        .expect("router's own pipeline accepts this");
    let adapted_shared =
        parse_via_apollo_configuration(text, &router_options(), &Expansion::builder().build())
            .expect("the adapter accepts this");

    let router_yaml = router
        .validated_yaml
        .as_ref()
        .expect("router populates validated_yaml");
    let shared_yaml = adapted_shared
        .validated_yaml
        .as_ref()
        .expect("the adapter populates validated_yaml");

    for (path, expected_value) in [
        ("$.batching", None),
        ("$.persisted_queries", None),
        ("$.subscription.enabled", Some(json!(true))),
    ] {
        let router_hit = jsonpath_lib::selector(router_yaml)(path)
            .unwrap_or_else(|error| panic!("[{path}] valid JSONPath on router's side: {error}"))
            .first()
            .copied()
            .cloned();
        let shared_hit = jsonpath_lib::selector(shared_yaml)(path)
            .unwrap_or_else(|error| panic!("[{path}] valid JSONPath on the shared side: {error}"))
            .first()
            .copied()
            .cloned();

        assert_eq!(
            router_hit.is_some(),
            shared_hit.is_some(),
            "[{path}] licence-restriction presence check must agree"
        );
        if let Some(expected) = expected_value {
            assert_eq!(router_hit, Some(expected.clone()), "[{path}] router side");
            assert_eq!(shared_hit, Some(expected), "[{path}] shared side");
        }
    }
}
