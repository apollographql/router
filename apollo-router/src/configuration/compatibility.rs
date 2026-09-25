//! Regression corpus for configuration loading with apollo-configuration.
//!
//! These fixtures were compared against the router's previous loader before it was removed, and
//! both produced the same effective settings. The snapshots record what each fixture changes
//! from the default configuration, so a change in loading behaviour shows up as a snapshot
//! difference.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;
use serde_json::json;

use super::Configuration;
use super::ConfigurationError;
use super::apollo_configuration_parse::Migration;
use super::apollo_configuration_parse::parse_configuration;
use super::expansion::Expansion;
use super::expansion::Override;
use super::expansion::ValueType;
use super::schema::router_config_schema;
use super::upgrade::UpgradeMode;
use super::upgrade::upgrade_configuration;
use crate::plugins::healthcheck::Config as HealthCheck;
use crate::plugins::subscription::SubscriptionConfig;
use crate::spec::Schema;
use crate::test_harness::tracing_test;
use crate::uplink::license_enforcement::LicenseEnforcementReport;
use crate::uplink::license_enforcement::LicenseState;

/// How an operator brings a corpus fixture into a loadable shape.
#[derive(Clone, Copy)]
enum Upgrade {
    /// Already in the current shape, or migrated automatically at startup.
    Startup,
    /// Needs a migration outside the current major version. Startup rejects this fixture;
    /// only `router config upgrade` (`UpgradeMode::Major`) can bring it forward.
    Command,
}

/// One corpus fixture. `name` identifies the case in a failing assertion, and `snapshot` names
/// its snapshot, independently of the file name.
struct Case {
    name: &'static str,
    snapshot: &'static str,
    text: &'static str,
    upgrade: Upgrade,
}

const FEATUREFUL_CASE: Case = Case {
    name: "cors policies, apq, persisted queries, batching, limits, health check, \
           subscription, a hidden built-in plugin and a custom plugin",
    snapshot: "current_featureful",
    text: include_str!("testdata/compat/current_featureful.yaml"),
    upgrade: Upgrade::Startup,
};

const CASES: &[Case] = &[
    Case {
        name: "minimal supergraph listener",
        snapshot: "current_minimal",
        text: include_str!("testdata/compat/current_minimal.yaml"),
        upgrade: Upgrade::Startup,
    },
    FEATUREFUL_CASE,
    Case {
        name: "batching integration configuration",
        snapshot: "batching_all_enabled",
        text: include_str!("../../tests/fixtures/batching/all_enabled.router.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "documented persisted-query safelist configuration",
        snapshot: "safelist_pq_require_id",
        text: include_str!("../../../examples/persisted-queries/safelist_pq_require_id.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "commercial configuration: every licence-restricted path \
               (authentication, authorization, batching, coprocessor, demand control, \
               persisted queries, subscriptions, the restricted plugin, response and \
               query-plan caching in Redis, and all four operation limits)",
        snapshot: "current_commercial",
        text: include_str!("testdata/compat/current_commercial.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "enhanced_client_awareness and experimental_diagnostics top-level keys",
        snapshot: "current_client_awareness_and_diagnostics",
        text: include_str!("testdata/compat/current_client_awareness_and_diagnostics.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "telemetry exporters and instrumentation",
        snapshot: "tracing_config",
        text: include_str!("testdata/tracing_config.router.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "cors.origins migrates into cors.policies",
        snapshot: "needs_minor_migration_cors_origins",
        text: include_str!("testdata/compat/needs_minor_migration_cors_origins.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "flat headers.all.request migrates under an operations key",
        snapshot: "needs_minor_migration_headers_flat_list",
        text: include_str!("testdata/compat/needs_minor_migration_headers_flat_list.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "flat subscription.deduplication migrates under deduplication.all",
        snapshot: "subscription_dedup_subgraph",
        text: include_str!("testdata/migrations/subscription_dedup_subgraph.yaml"),
        upgrade: Upgrade::Startup,
    },
    Case {
        name: "experimental_batching renamed to batching (breaking; needs `router config upgrade`)",
        snapshot: "batching_major_migration",
        text: include_str!("testdata/migrations/batching.yaml"),
        upgrade: Upgrade::Command,
    },
];

fn parse(text: &str) -> Result<Configuration, ConfigurationError> {
    parse_configuration(text, Expansion::builder().build(), Migration::WithinMajor)
}

/// Loads `case` the way an operator would run it.
fn load(case: &Case) -> Result<Configuration, ConfigurationError> {
    match case.upgrade {
        Upgrade::Startup => parse(case.text),
        Upgrade::Command => {
            let original: Value = serde_yaml::from_str(case.text).expect("fixtures are YAML");
            let upgraded = upgrade_configuration(&original, false, UpgradeMode::Major)?;
            let upgraded = serde_yaml::to_string(&upgraded).expect("migrations serialize");
            parse_configuration(&upgraded, Expansion::builder().build(), Migration::None)
        }
    }
}

/// Every setting in `config` that differs from the default configuration, keyed by JSON pointer.
fn changed_settings(config: &Configuration) -> BTreeMap<String, Value> {
    fn walk(
        default: Option<&Value>,
        value: &Value,
        path: &mut String,
        changes: &mut BTreeMap<String, Value>,
    ) {
        match (default, value) {
            (Some(Value::Object(default)), Value::Object(entries)) => {
                for (key, entry) in entries {
                    let prefix_len = path.len();
                    path.push('/');
                    path.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    walk(default.get(key), entry, path, changes);
                    path.truncate(prefix_len);
                }
            }
            (Some(default), value) if default == value => {}
            _ => {
                changes.insert(path.clone(), value.clone());
            }
        }
    }

    // Configuration::eq compares only validated_yaml. Serialize to compare effective settings.
    let default = serde_json::to_value(parse("").expect("the default configuration is valid"))
        .expect("Configuration serializes");
    let value = serde_json::to_value(config).expect("Configuration serializes");
    let mut changes = BTreeMap::new();
    walk(Some(&default), &value, &mut String::new(), &mut changes);
    changes
}

#[test]
fn changed_settings_name_each_setting() {
    let config = parse(
        "supergraph:\n  listen: 127.0.0.1:4001\n  query_planning:\n    cache:\n      redis:\n        urls: [\"redis://localhost\"]\n",
    )
    .expect("the fixture is valid");

    let changes = changed_settings(&config);

    assert_eq!(changes["/supergraph/listen"], json!("127.0.0.1:4001"));
    let redis = &changes["/supergraph/query_planning/cache/redis"];
    assert_eq!(redis["urls"], json!(["redis://localhost"]));
}

/// Each fixture, loaded as an operator would load it, keeps the effective settings recorded in
/// its snapshot.
#[test]
fn corpus_effective_settings() {
    for case in CASES {
        let config = load(case).unwrap_or_else(|error| {
            panic!(
                "[{}] loading rejected a fixture expected to succeed: {error}",
                case.name
            )
        });
        insta::with_settings!({ snapshot_suffix => case.snapshot, description => case.name }, {
            insta::assert_json_snapshot!(changed_settings(&config));
        });
    }
}

#[test]
fn schema_derived_boolean_values_are_applied() {
    let schema = router_config_schema();
    let default = schema["properties"]["experimental_type_conditioned_fetching"]["default"]
        .as_bool()
        .expect("the schema declares a boolean default");
    for enabled in [default, !default] {
        let config = parse(&format!(
            "experimental_type_conditioned_fetching: {enabled}\n"
        ))
        .expect("schema-derived input is valid");
        assert_eq!(config.experimental_type_conditioned_fetching, enabled);
    }
}

/// An unknown top-level key is a schema violation, because the parser uses router's patched root
/// schema.
#[test]
fn unknown_top_level_key_is_rejected() {
    let error = parse(include_str!("testdata/compat/unknown_top_level_key.yaml"))
        .expect_err("the key is not declared")
        .to_string();
    assert!(
        error.contains("this_key_does_not_exist_anywhere"),
        "{error}"
    );
}

/// An unregistered plugin name is rejected: `UserPlugins`'s generated schema sets
/// `additionalProperties: false`.
#[test]
fn unknown_plugin_name_is_rejected() {
    parse(include_str!("testdata/compat/unknown_plugin_name.yaml"))
        .expect_err("the plugin is not registered");
}

/// Duplicate keys are rejected in the original text, before migration or parsing could
/// collapse them.
#[test]
fn duplicate_keys_are_rejected_before_migration() {
    let error = parse(include_str!("testdata/compat/duplicate_keys.yaml"))
        .expect_err("migration must not erase duplicate keys");
    assert!(error.to_string().contains("duplicated keys"), "{error}");
}

/// Configuration-usage selectors and licence checks read `validated_yaml`, which holds the
/// expanded document with overrides applied.
#[test]
fn validated_yaml_carries_expansion_and_overrides_for_usage_selectors() {
    let text = "persisted_queries:\n  enabled: ${env.COMPAT_PERSISTED_QUERIES}\n";
    let expansion = Expansion::builder()
        .supported_mode("env")
        .mocked_env_var("COMPAT_PERSISTED_QUERIES", "true")
        .override_config(
            Override::builder()
                .config_path("subscription.enabled")
                .env_name("COMPAT_SUBSCRIPTION")
                .mocked_env_var("COMPAT_SUBSCRIPTION", "true")
                .value_type(ValueType::Bool)
                .build(),
        )
        .build();
    let config =
        parse_configuration(text, expansion, Migration::None).expect("the configuration is valid");

    let document = config.validated_yaml.as_ref().unwrap();
    assert_eq!(document["persisted_queries"]["enabled"], json!(true));
    assert_eq!(document["subscription"]["enabled"], json!(true));
    let selector =
        jsonpath_rust::JsonPathInst::from_str("$.persisted_queries[?(@.enabled == true)]")
            .expect("valid path");
    assert_eq!(
        selector.find_slice(document).len(),
        1,
        "the usage gauge for `apollo.router.config.persisted_queries` must fire"
    );
    assert_eq!(
        config.apollo_plugins.plugins["subscription"]["enabled"],
        json!(true)
    );
    assert_eq!(config.raw_yaml.as_deref(), Some(text));
}

#[test]
fn migrated_documents_keep_the_original_text_as_raw_yaml() {
    let text = include_str!("testdata/compat/needs_minor_migration_cors_origins.yaml");
    let config = parse(text).expect("the router migrates legacy CORS settings");
    let document = config.validated_yaml.as_ref().unwrap();
    assert!(document["cors"].get("origins").is_none());
    assert!(document["cors"].get("policies").is_some());
    assert_eq!(config.raw_yaml.as_deref(), Some(text));
}

#[test]
fn unknown_key_is_rejected_before_and_after_migration() {
    let text = "cors:\n  origins:\n    - \"https://example.com\"\nthis_key_does_not_exist_anywhere: true\n";
    let error = parse(text).expect_err("the key is unknown in either form");
    assert!(
        error
            .to_string()
            .contains("this_key_does_not_exist_anywhere"),
        "{error}"
    );
}

#[test]
fn introspection_defaults_and_explicit_values_are_applied() {
    let previous = parse(include_str!("testdata/compat/current_minimal.yaml")).unwrap();
    let next = parse(include_str!("testdata/compat/current_minimal_v2.yaml")).unwrap();

    assert!(!previous.supergraph.introspection);
    assert!(next.supergraph.introspection);
}

/// Mandatory plugin defaults (`limits`, `health_check`) are present even when the document never
/// mentions them.
#[test]
fn mandatory_plugin_defaults_are_present_without_being_configured() {
    let config = parse(include_str!("testdata/compat/current_minimal.yaml")).unwrap();

    for plugin in ["limits", "health_check"] {
        assert!(
            config.apollo_plugins.plugins.contains_key(plugin),
            "the mandatory `{plugin}` plugin entry must be defaulted"
        );
        assert!(
            config.plugin_config(&format!("apollo.{plugin}")).is_some(),
            "the mandatory `{plugin}` plugin config must be kept"
        );
    }
}

/// Parsing keeps typed plugin config that matches the document, and plugins are constructed
/// from it.
#[tokio::test]
async fn typed_plugin_configs_are_retained_and_construct_plugins() {
    let config = load(&FEATUREFUL_CASE).expect("the featureful fixture is valid");

    let subscription: SubscriptionConfig = config
        .plugin_config("apollo.subscription")
        .expect("subscription config is kept")
        .typed()
        .unwrap();
    let health_check: HealthCheck = config
        .plugin_config("apollo.health_check")
        .expect("health check config is kept")
        .typed()
        .unwrap();
    let document: SubscriptionConfig =
        serde_json::from_value(config.apollo_plugins.plugins["subscription"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&subscription).unwrap(),
        serde_json::to_value(&document).unwrap(),
        "typed subscription config, including deduplication, must match the document"
    );
    assert_eq!(
        serde_json::to_value(&health_check).unwrap(),
        serde_json::to_value(&config.health_check).unwrap()
    );

    for name in ["apollo.health_check", "apollo.subscription"] {
        let factory = crate::plugin::plugins()
            .find(|factory| factory.name == name)
            .expect("built-in plugin is registered");
        let retained = config.plugin_config(name).unwrap().clone();
        factory
            .create_from_config(
                crate::plugin::PluginInit::fake_builder()
                    .config(())
                    .build()
                    .with_config(retained, None),
            )
            .await
            .unwrap_or_else(|error| panic!("{name} initialization failed: {error}"));
    }
}

/// The flat deduplication shape passes schema validation but fails typed deserialization.
/// Startup migrates it under `deduplication.all`; without migration, parsing rejects it before
/// any plugin is constructed.
#[test]
fn unmigrated_flat_subscription_dedup_is_rejected_while_parsing() {
    let text = include_str!("testdata/migrations/subscription_dedup_subgraph.yaml");

    parse(text).expect("startup migrates the flat shape");
    let error = parse_configuration(text, Expansion::builder().build(), Migration::None)
        .expect_err("typed deserialization rejects the unmigrated flat shape")
        .to_string();
    assert!(error.contains("apollo.subscription"), "{error}");
}

/// Cross-field validation runs inside `Configuration`'s deserializer.
#[test]
fn cross_field_validation_rejects_sandbox_with_homepage() {
    let error = parse(
        "sandbox:\n  enabled: true\nhomepage:\n  enabled: true\nsupergraph:\n  introspection: true\n",
    )
    .expect_err("sandbox and homepage cannot both be enabled");
    assert!(
        error
            .to_string()
            .contains("sandbox and homepage cannot be enabled"),
        "{error}"
    );
}

/// Intentional difference from the previous loader, which fell back to the original document
/// only when the migrated one failed the schema check. apollo-configuration validates in one call,
/// so a migrated document rejected after the schema check, here by a plugin's config, falls back
/// too. Startup migration 2045 fixes the flat deduplication settings, the migrated copy then
/// fails on the traffic shaping timeout, and so does the file as written.
#[test]
fn migrated_document_failing_plugin_config_falls_back_to_the_file() {
    let _guard = tracing_test::dispatcher_guard();

    let error = parse(include_str!(
        "testdata/compat/fallback_after_plugin_config_error.yaml"
    ))
    .expect_err("the traffic shaping timeout is invalid")
    .to_string();

    assert!(error.contains("apollo.traffic_shaping"), "{error}");
    // Only the operator's file has this comment; the serialized migrated copy has none.
    assert!(
        error.contains("# The schema accepts any string here"),
        "the diagnostic should quote the operator's file: {error}"
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

/// The sandbox checks run once the document has been parsed, so a migrated document that
/// enables both sandbox and homepage is still rejected, without falling back.
#[test]
fn sandbox_conflicts_are_rejected_after_migration() {
    let error = parse(
        "subscription:\n  deduplication:\n    enabled: true\nsandbox:\n  enabled: true\nhomepage:\n  enabled: true\nsupergraph:\n  introspection: true\n",
    )
    .expect_err("sandbox and homepage cannot both be enabled");
    assert!(
        error
            .to_string()
            .contains("sandbox and homepage cannot be enabled"),
        "{error}"
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

/// An override replaces an explicit value in the document.
#[test]
fn an_env_var_override_beats_the_documents_own_value() {
    let expansion = Expansion::builder()
        .override_config(
            Override::builder()
                .config_path("supergraph.listen")
                .env_name("COMPAT_TEST_LISTEN_OVERRIDE")
                .value_type(ValueType::String)
                .mocked_env_var("COMPAT_TEST_LISTEN_OVERRIDE", "127.0.0.1:9999")
                .build(),
        )
        .build();
    let config = parse_configuration(
        "supergraph:\n  listen: 127.0.0.1:4000\n",
        expansion,
        Migration::None,
    )
    .expect("the override applies");

    assert_eq!(
        config.validated_yaml.unwrap()["supergraph"]["listen"],
        json!("127.0.0.1:9999")
    );
    assert_eq!(
        config.supergraph.listen.to_string(),
        "http://127.0.0.1:9999"
    );
}

/// An invalid override is reported against the environment variable that supplied it.
#[test]
fn an_invalid_override_names_its_environment_variable() {
    let expansion = Expansion::builder()
        .override_config(
            Override::builder()
                .config_path("supergraph.introspection")
                .env_name("COMPAT_TEST_INTROSPECTION")
                .value_type(ValueType::String)
                .mocked_env_var("COMPAT_TEST_INTROSPECTION", "sometimes")
                .build(),
        )
        .build();
    let error = parse_configuration("", expansion, Migration::None)
        .expect_err("introspection must be a boolean")
        .to_string();

    assert!(error.contains("COMPAT_TEST_INTROSPECTION"), "{error}");
}

#[test]
fn file_expansion_resolves_a_root_level_field() {
    let mut file = tempfile::NamedTempFile::new().expect("can create a temp file");
    std::io::Write::write_all(&mut file, b"true").expect("can write the temp file");
    let text = format!(
        "experimental_type_conditioned_fetching: ${{file.{}}}\n",
        file.path().to_string_lossy()
    );

    let config = parse_configuration(
        &text,
        Expansion::builder().supported_mode("file").build(),
        Migration::None,
    )
    .expect("file expansion resolves this");

    assert!(config.experimental_type_conditioned_fetching);
}

/// File expansion coerces a nested boolean through the schema's `allOf` reference wrapper.
#[test]
fn file_expansion_boolean_coercion_resolves_through_a_nested_allof_ref() {
    let mut file = tempfile::NamedTempFile::new().expect("can create a temp file");
    std::io::Write::write_all(&mut file, b"true").expect("can write the temp file");
    let text = format!(
        "supergraph:\n  introspection: ${{file.{}}}\n",
        file.path().to_string_lossy()
    );

    let config = parse_configuration(
        &text,
        Expansion::builder().supported_mode("file").build(),
        Migration::None,
    )
    .expect("the nested field's declared type resolves the coercion");

    assert!(config.supergraph.introspection);
}

/// File expansion removes one trailing newline, so values written with `echo` expand to the
/// file's text.
#[test]
fn file_expansion_removes_one_trailing_newline() {
    let mut file = tempfile::NamedTempFile::new().expect("can create a temp file");
    std::io::Write::write_all(&mut file, b"https://example.com/usage\n")
        .expect("can write the temp file");
    let text = format!(
        "telemetry:\n  apollo:\n    endpoint: \"${{file.{}}}\"\n",
        file.path().to_string_lossy()
    );

    let config = parse_configuration(
        &text,
        Expansion::builder().supported_mode("file").build(),
        Migration::None,
    )
    .expect("file expansion resolves this");

    assert_eq!(
        config.validated_yaml.unwrap()["telemetry"]["apollo"]["endpoint"],
        json!("https://example.com/usage")
    );
}

/// Setting a top-level boolean, string or number to the default its schema declares has the
/// same effect as omitting it.
#[test]
fn schema_declared_top_level_defaults_match_the_default_configuration() {
    let properties = router_config_schema()["properties"]
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
        let config = parse(&format!("{key}: {default}\n")).unwrap_or_else(|error| {
            panic!("[{key}] loading rejected its own schema-declared default: {error}")
        });
        let changes = changed_settings(&config);
        assert!(
            changes.is_empty(),
            "[{key}, set to the schema's own declared default] changed {changes:?}"
        );
    }

    assert!(
        checked > 0,
        "expected at least one top-level property with a scalar schema default"
    );
}

/// Rate limits with different capacities and intervals are applied.
#[test]
fn rate_limit_settings_are_applied() {
    for (capacity, interval) in [(10, "1s"), (500, "30s")] {
        let config = parse(&format!(
            "traffic_shaping:\n  all:\n    global_rate_limit:\n      capacity: {capacity}\n      interval: {interval}\n"
        ))
        .unwrap_or_else(|error| {
            panic!("[capacity={capacity}, interval={interval}] loading rejected valid settings: {error}")
        });
        let changes = changed_settings(&config);
        assert_eq!(
            changes["/traffic_shaping"]["all"]["global_rate_limit"],
            json!({ "capacity": capacity, "interval": interval }),
            "{changes:?}"
        );
    }
}

/// Licence enforcement reads `validated_yaml` and flags every restricted feature the commercial
/// fixture enables, so an unlicensed router refuses it.
#[test]
fn licence_enforcement_flags_the_commercial_configuration() {
    let text = include_str!("testdata/compat/current_commercial.yaml");
    let schema_sdl = include_str!("../uplink/testdata/oss.graphql");
    let config = parse(text).expect("the commercial fixture is valid");
    let schema = Schema::parse(schema_sdl, &config).expect("the schema parses");

    let report =
        LicenseEnforcementReport::build(&config, &schema, Arc::new(LicenseState::Unlicensed));

    let mut features = report.restricted_features_in_use();
    features.sort();
    insta::assert_json_snapshot!(features);
    assert!(report.enforce().is_err());
}
