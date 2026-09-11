//! Compares the router's own configuration loader against the shared `apollo-configuration`
//! crate that ROUTER-2104 will replace it with.
//!
//! This module never runs outside tests and nothing here ships. It exists so the swap in
//! ROUTER-2104 has evidence, gathered beforehand, that running the same YAML through both
//! parsers produces the same effective settings and the same plugin configuration.
//!
//! `Configuration` already implements two of the three traits `apollo_configuration::Configuration`
//! requires -- `JsonSchema` and `Deserialize` -- so the marker impls below let the shared
//! crate parse it directly, with no parallel struct tree to keep in sync. Its own `Deserialize`
//! impl already calls `Configuration::validate()` at the end, so every cross-field invariant
//! router enforces today (sandbox vs. homepage, persisted-queries safelisting, the mandatory
//! `limits`/`health_check` plugin entries) runs identically however the document reaches it.

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
use super::upgrade::UpgradeMode;
use super::upgrade::upgrade_configuration;
use crate::plugins::healthcheck::Config as HealthCheck;
use crate::plugins::subscription::SubscriptionConfig;

impl apollo_configuration::Validate for Configuration {}
impl apollo_configuration::Configuration for Configuration {}

fn current_major_version() -> i64 {
    env!("CARGO_PKG_VERSION_MAJOR")
        .parse()
        .expect("CARGO_PKG_VERSION_MAJOR should be an integer")
}

/// Options for the shared parser configured the way ROUTER-2104's loader will need to be.
///
/// Router patches `additionalProperties: false` onto the schema root after generation
/// (`schema::generate_config_schema`), because `Configuration` can't carry
/// `#[serde(deny_unknown_fields)]` itself -- it has a `#[serde(flatten)]` field, and serde
/// doesn't support combining the two. Reusing `generate_config_schema` directly, rather than
/// re-deriving that patch here, means an unknown top-level key is rejected the same way on both
/// sides by construction, not by coincidence.
fn router_options() -> ParseYamlOptions {
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

/// Parses `case` through the shared crate. `apollo_configuration` has no concept of router's
/// migrations, so a fixture needing one is run through router's own `upgrade_configuration`
/// first -- the same function `router_effective_settings` uses for a major migration, and the
/// same transformation router's own `Mode::Upgrade` applies internally for a minor one. This
/// mirrors what ROUTER-2104's loader will actually do: migrate, then hand the result to the
/// shared parser, rather than expecting the shared crate to know router's migration history.
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
///
/// Panicking with the whole serialized configuration on a mismatch works, but forces the reader
/// to scan two large JSON documents by eye for the one field that differs. This walks both trees
/// together and stops at the first disagreement, so a failing comparison can name it directly.
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
                    path.push_str(key);
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

/// Named difference: both parsers reject a document with duplicated top-level keys, but by
/// different means and with different messages. Router's own YAML pre-pass (`yaml::parse`)
/// rejects the duplicate before migration or schema validation ever run, with a message naming
/// the duplicated key (`startup_reports_duplicate_keys_in_a_document_that_also_migrates` in
/// `tests.rs`). The shared crate has no equivalent pre-pass of its own; `parse_yaml` calls
/// `serde_yaml::from_str` directly, whose duplicate-key check produces a plain YAML syntax error
/// instead. Neither accepts the document, so there is no silent-acceptance gap here -- only a
/// wording difference for ROUTER-2104 to be aware of if anything downstream matches on router's
/// current message text.
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

/// Parses through the shared crate the way ROUTER-2104's loader will need to, so the
/// `apollo.router.config.*` usage gauges keep working after the swap (see the test above).
/// `apollo_configuration` doesn't expose the post-expansion document `parse_yaml` builds
/// internally, so this computes it independently with router's own `Expansion` -- the same
/// expansion step `validate_yaml_configuration` already performs -- and stores it on the result.
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

/// A migrated document that still fails validation stops startup -- the loader does not retry
/// the un-migrated original. Reload runs through the same `validate_yaml_configuration` call:
/// this proves the parsing-level precondition an invalid reload relies on, an `Err` rather than a
/// silently accepted fallback document. What happens to the request-serving pipeline after that
/// `Err` -- staying on the previously accepted configuration -- is `state_machine.rs`'s job, not
/// this module's.
#[test]
fn an_invalid_migrated_replacement_does_not_fall_back_to_the_original() {
    let invalid_reload_text = "cors:\n  origins:\n    - \"https://example.com\"\nthis_key_does_not_exist_anywhere: true\n";
    let reload_error = validate_yaml_configuration(
        invalid_reload_text,
        Expansion::builder().build(),
        Mode::Upgrade,
    )
    .expect_err("a migrated document that still fails validation must stop the reload");
    assert!(reload_error.to_string().contains(
        "Additional properties are not allowed ('this_key_does_not_exist_anywhere' was unexpected)"
    ),);
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

/// Stands in for plugin construction. Its signature has no `serde_json::Value` parameter, so the
/// only way to call it is with values a parsing step already typed -- it cannot deserialize the
/// raw config again.
fn plugins_from_typed_configs(typed: TypedApolloPlugins) -> (HealthCheck, SubscriptionConfig) {
    (typed.health_check, typed.subscription)
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
#[test]
fn typed_plugin_configs_agree_and_construction_reuses_them_without_reparsing() {
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

    let router_config_for_comparison = serde_json::to_value(&router_typed.subscription).unwrap();
    let (_constructed_health_check, constructed_subscription) =
        plugins_from_typed_configs(router_typed);
    assert_eq!(
        serde_json::to_value(&constructed_subscription).unwrap(),
        router_config_for_comparison,
        "construction must reuse the exact typed value parsing produced"
    );
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

/// `Configuration::validate` (above) is invariants baked into router's `Deserialize` impl, not
/// `apollo_configuration`'s own validation mechanism. ROUTER-2104's plugin configs will instead
/// use `#[configuration(validate = ...)]`, so this proves that mechanism out directly, on a
/// small, self-contained type unrelated to any router struct: a schema-valid document can still
/// fail a custom validator, and the shared parse call is what rejects it.
mod custom_plugin_validation {
    use apollo_configuration::ErrorCollector;
    use apollo_configuration::ParseYamlOptions;
    use apollo_configuration::configuration;
    use miette::Diagnostic as _;

    /// A stand-in for a future ROUTER-2104 plugin config with cross-field validation: both
    /// fields are individually valid integers (so JSON Schema accepts them), but the custom
    /// validator additionally requires each to be nonzero.
    #[configuration(validate = validate_widget)]
    struct WidgetConfig {
        #[config(default = 1)]
        #[allow(dead_code)]
        replica_count: u32,
        #[config(default = 1)]
        #[allow(dead_code)]
        shard_count: u32,
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
        ParseYamlOptions::default()
            .parse::<WidgetConfig>(yaml)
            .expect_err("zero replicas passes the schema but fails custom validation");
    }

    /// Multiple custom-validation errors on nested fields are all reported, each labeled at its
    /// own location in the source document rather than collapsed into one error at the root.
    #[test]
    fn multiple_custom_validation_errors_keep_distinct_nested_locations() {
        let yaml = "replica_count: 0\nshard_count: 0\n";
        let error = ParseYamlOptions::default()
            .parse::<WidgetConfig>(yaml)
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

        let spans: Vec<_> = related
            .iter()
            .flat_map(|diagnostic| diagnostic.labels().into_iter().flatten())
            .collect();
        assert_eq!(spans.len(), 2, "each error carries its own labeled span");
        assert_ne!(
            spans[0].offset(),
            spans[1].offset(),
            "the two errors must point at their own field, not both at the same location"
        );
    }
}

/// Router's `Expansion::expand_env` and the shared crate's expansion resolve `${env.NAME}`
/// identically. Both sides use their own mocking seam (`Expansion::mocked_env_vars`,
/// `apollo_configuration::expansion::MapVariables`) rather than `std::env::set_var`, so this
/// test cannot race another test over the same process environment.
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

/// `${file.PATH}` expansion reads a real file on both sides -- there is no mocking seam for it
/// because reading an actual temp file this test creates and controls is no less deterministic
/// than mocking would be, and it exercises the real filesystem code path on both parsers.
///
/// This targets `experimental_type_conditioned_fetching`, a plain `bool` declared directly on
/// `Configuration`, rather than a field nested inside another struct -- see the known-gap test
/// below for why the distinction matters to whole-value boolean coercion specifically.
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

/// The shared parser leaves the expanded `supergraph.introspection` value as a string;
/// router converts it to a boolean. Shared coercion in version 0.6.1 cannot traverse the
/// `allOf` wrapper around the schema's `Supergraph` reference.
#[test]
fn file_expansion_boolean_coercion_does_not_resolve_through_a_nested_allof_ref() {
    let mut file = tempfile::NamedTempFile::new().expect("can create a temp file");
    std::io::Write::write_all(&mut file, b"true").expect("can write the temp file");
    let text = format!(
        "supergraph:\n  introspection: ${{file.{}}}\n",
        file.path().to_string_lossy()
    );

    let router_expansion = Expansion::builder().supported_mode("file").build();
    let router_config = validate_yaml_configuration(&text, router_expansion, Mode::NoUpgrade)
        .expect("router's own file expansion always coerces, so this succeeds");
    assert!(router_config.supergraph.introspection);

    let shared_options = router_options().add_variables(FileVariables);
    let shared_error = shared_options.parse::<Configuration>(&text).expect_err(
        "the shared crate leaves the expanded value as the string \"true\" here, which then \
             fails the schema's boolean check",
    );
    // `ConfigError::ValidationError` displays a fixed "schema validation error" summary; the
    // per-field message lives on the related diagnostics `miette::Diagnostic::related` exposes.
    let related_message = miette::Diagnostic::related(&shared_error)
        .into_iter()
        .flatten()
        .map(|diagnostic| diagnostic.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        related_message.contains("is not of type \"boolean\""),
        "expected a boolean-type schema error, got: {related_message}"
    );
}

/// A minimal telemetry input -- reusing an existing broad telemetry fixture rather than adding a
/// new one to this corpus -- produces the same effective settings through both parsers.
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
