//! Which plugins a built pipeline instantiates: mandatory plugins always, and every other
//! plugin whenever it is configured.

use std::sync::Arc;

use rstest::rstest;
use serde_json::json;
use tower_http::BoxError;

use crate::configuration::Configuration;
use crate::pipeline::plugins::create_plugins;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::router_factory::PipelineFactory;
use crate::router_factory::RouterServiceFactory;
use crate::services::supergraph::service::Plugins;
use crate::spec::Schema;

const MANDATORY_PLUGINS: &[&str] = &[
    "apollo.include_subgraph_errors",
    "apollo.headers",
    "apollo.health_check",
    "apollo.traffic_shaping",
    "apollo.limits",
    "apollo.csrf",
    "apollo.fleet_detector",
    "apollo.enhanced_client_awareness",
    "apollo.progressive_override",
    "apollo.authorization",
];

const OSS_PLUGINS: &[&str] = &[
    "apollo.forbid_mutations",
    "apollo.override_subgraph_url",
    "apollo.connectors",
];

// Always starts and stops plugin

#[derive(Debug)]
struct AlwaysStartsAndStopsPlugin {}

/// Configuration for the test plugin
#[apollo_configuration::configuration]
#[derive(Default)]
struct Conf {
    /// The name of the test
    #[config(required)]
    name: String,
}

#[async_trait::async_trait]
impl Plugin for AlwaysStartsAndStopsPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::debug!("{}", init.config.name);
        Ok(AlwaysStartsAndStopsPlugin {})
    }
}

register_plugin!(
    "test",
    "always_starts_and_stops",
    AlwaysStartsAndStopsPlugin
);

// Always fails to start plugin

#[derive(Debug)]
struct AlwaysFailsToStartPlugin {}

#[async_trait::async_trait]
impl Plugin for AlwaysFailsToStartPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::debug!("{}", init.config.name);
        Err(BoxError::from("Error"))
    }
}

register_plugin!("test", "always_fails_to_start", AlwaysFailsToStartPlugin);

// Records the previous config it is built with

/// The previous config the last built `test.records_previous_config` received.
static PREVIOUS_CONFIG: parking_lot::Mutex<Option<Option<String>>> = parking_lot::Mutex::new(None);

#[derive(Debug)]
struct RecordsPreviousConfigPlugin {}

#[async_trait::async_trait]
impl Plugin for RecordsPreviousConfigPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        *PREVIOUS_CONFIG.lock() = Some(init.previous_config.map(|previous| previous.name));
        Ok(RecordsPreviousConfigPlugin {})
    }
}

register_plugin!(
    "test",
    "records_previous_config",
    RecordsPreviousConfigPlugin
);

async fn create_service(config: Configuration) -> Result<(), BoxError> {
    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &config)?;

    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(config),
            Arc::new(schema),
            None,
            None,
        )
        .await;
    service.map(|_| ())
}

/// Plugins registered through the public API have their config deserialized when the
/// configuration is parsed, like built-in plugins, so each construction reuses the validated value.
#[test]
fn user_plugin_config_is_deserialized_when_parsed() {
    let config: Configuration =
        "plugins:\n  test.always_starts_and_stops:\n    name: parsed once\n"
            .parse()
            .expect("the plugin config is valid");

    let plugin_config: Conf = config
        .plugins
        .get("test.always_starts_and_stops")
        .expect("the plugin's config is kept")
        .config
        .typed()
        .expect("the config was deserialized during parsing");
    assert_eq!(plugin_config.name, "parsed once");
}

/// Construction takes user plugins from their parsed config, in configuration order. A section
/// naming no registered plugin is rejected while parsing, whichever way the configuration is
/// built.
#[test]
fn user_plugins_are_built_from_their_parsed_config() {
    let config: Configuration = yaml_serde::from_str(
        "plugins:\n  test.always_fails_to_start:\n    name: first\n  test.always_starts_and_stops:\n    name: second\n",
    )
    .expect("the user plugin sections deserialize");

    let names: Vec<&str> = config.plugins.iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        ["test.always_fails_to_start", "test.always_starts_and_stops"]
    );
    yaml_serde::from_str::<Configuration>("plugins:\n  acme.unregistered: {}\n")
        .expect_err("no plugin is registered as acme.unregistered");
}

/// On a hot reload, each plugin is built with the config it ran with before.
#[tokio::test]
async fn plugins_receive_their_previous_config_on_reload() {
    let configuration = |name: &str| -> Configuration {
        format!("plugins:\n  test.records_previous_config:\n    name: {name}\n")
            .parse()
            .expect("the plugin config is valid")
    };
    let previous = configuration("before");
    let current = configuration("after");
    let schema = Schema::parse(include_str!("../../testdata/supergraph.graphql"), &current)
        .expect("the supergraph is valid");

    create_plugins(
        &current,
        &schema,
        Default::default(),
        None,
        None,
        Some(Arc::new(previous)),
    )
    .await
    .expect("the plugins build");

    assert_eq!(*PREVIOUS_CONFIG.lock(), Some(Some("before".to_string())));
}

#[tokio::test]
async fn test_yaml_no_extras() {
    let config = Configuration::builder().build().unwrap();
    let service = create_service(config).await;
    assert!(service.is_ok())
}

#[tokio::test]
async fn test_yaml_plugins_always_starts_and_stops() {
    let config: Configuration = yaml_serde::from_str(
        r#"
            plugins:
                test.always_starts_and_stops:
                    name: albert
        "#,
    )
    .unwrap();
    let service = create_service(config).await;
    assert!(service.is_ok())
}

#[tokio::test]
async fn test_yaml_plugins_always_fails_to_start() {
    let config: Configuration = yaml_serde::from_str(
        r#"
            plugins:
                test.always_fails_to_start:
                    name: albert
        "#,
    )
    .unwrap();
    let service = create_service(config).await;
    assert!(service.is_err())
}

#[tokio::test]
async fn test_yaml_plugins_combo_start_and_fail() {
    let config: Configuration = yaml_serde::from_str(
        r#"
            plugins:
                test.always_starts_and_stops:
                    name: albert
                test.always_fails_to_start:
                    name: albert
        "#,
    )
    .unwrap();
    let service = create_service(config).await;
    assert!(service.is_err())
}

/// The telemetry plugin reports against the supergraph schema's ID when Apollo reporting is
/// configured.
#[tokio::test(flavor = "multi_thread")]
async fn telemetry_reports_the_supergraph_schema_id() {
    let schema_id = "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8";
    let factory = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.telemetry")
        .expect("telemetry is registered");
    let plugin = factory
        .create_instance(
            PluginInit::fake_builder()
                .config(json!({ "apollo": {} }))
                .supergraph_schema_id(Arc::new(schema_id.to_string()))
                .full_config(json!({ "telemetry": { "apollo": {} } }))
                .build(),
        )
        .await
        .expect("telemetry builds");
    let telemetry = plugin
        .as_any()
        .downcast_ref::<crate::plugins::telemetry::Telemetry>()
        .expect("the telemetry plugin");
    assert_eq!(telemetry.config.apollo.schema_id, schema_id);
}

fn get_plugin_config(plugin: &str) -> &str {
    match plugin {
        "subscription" => {
            r#"
                enabled: true
                "#
        }
        "authentication" => {
            r#"
                connector:
                  sources: {}
                "#
        }
        "authorization" => {
            r#"
                require_authentication: false
                "#
        }
        "preview_file_uploads" => {
            r#"
                enabled: true
                protocols:
                  multipart:
                    enabled: false
                "#
        }
        "response_cache" => {
            r#"
                enabled: true
                subgraph:
                  all:
                    enabled: true
                "#
        }
        "demand_control" => {
            r#"
                enabled: true
                mode: measure
                strategy:
                  static_estimated:
                    list_size: 0
                    max: 0.0
                "#
        }
        "coprocessor" => {
            r#"
                url: http://service.example.com/url
                "#
        }
        "connectors" => {
            r#"
                debug_extensions: false
                "#
        }
        "experimental_mock_subgraphs" => {
            r#"
               subgraphs: {}
                "#
        }
        "forbid_mutations" => {
            r#"
                false
                "#
        }
        "override_subgraph_url" => {
            r#"
                {}
                "#
        }
        _ => panic!("This function does not contain config for plugin: {plugin}"),
    }
}

async fn build_pipeline_plugins(router_config: Configuration) -> Arc<Plugins> {
    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
        )
        .await
        .unwrap();
    service.plugins
}

fn plugin_config(plugin: &str) -> serde_json::Value {
    yaml_serde::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap()
}

#[tokio::test]
async fn test_mandatory_plugins_added() {
    let plugins = build_pipeline_plugins(Configuration::builder().build().unwrap()).await;

    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { plugins.contains_key(*plugin) })
    );
}

#[tokio::test]
async fn test_oss_plugins_added() {
    let router_config = Configuration::builder()
        .apollo_plugin("forbid_mutations", plugin_config("forbid_mutations"))
        .apollo_plugin(
            "override_subgraph_url",
            plugin_config("override_subgraph_url"),
        )
        .apollo_plugin("connectors", plugin_config("connectors"))
        .build()
        .unwrap();

    let plugins = build_pipeline_plugins(router_config).await;

    assert!(
        OSS_PLUGINS
            .iter()
            .all(|plugin| { plugins.contains_key(*plugin) })
    );
}

/// Building the pipeline doesn't check the license: the startup license check has already
/// refused any setting the license doesn't cover, so every configured plugin is built.
#[tokio::test]
#[rstest]
#[case::forbid_mutations("forbid_mutations")]
#[case::subscriptions("subscription")]
#[case::override_subgraph_url("override_subgraph_url")]
#[case::authorization("authorization")]
#[case::authentication("authentication")]
#[case::file_upload("preview_file_uploads")]
#[case::response_cache("response_cache")]
#[case::demand_control("demand_control")]
#[case::connectors("connectors")]
#[case::coprocessor("coprocessor")]
#[case::mock_subgraphs("experimental_mock_subgraphs")]
async fn test_configured_optional_plugin_added(#[case] plugin: &str) {
    let router_config = Configuration::builder()
        .apollo_plugin(plugin, plugin_config(plugin))
        .build()
        .unwrap();

    let plugins = build_pipeline_plugins(router_config).await;

    assert!(
        plugins.contains_key(&format!("apollo.{plugin}")),
        "Plugin {plugin} should have been added"
    );
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { plugins.contains_key(*plugin) })
    );
}

#[tokio::test]
async fn test_unconfigured_optional_plugins_not_added() {
    let router_config = Configuration::builder()
        .apollo_plugin(
            "experimental_mock_subgraphs",
            plugin_config("experimental_mock_subgraphs"),
        )
        .build()
        .unwrap();

    let plugins = build_pipeline_plugins(router_config).await;

    assert!(!plugins.contains_key("apollo.subscription"));
    assert!(!plugins.contains_key("apollo.coprocessor"));
}

#[tokio::test]
#[rstest]
#[case::forbid_mutations("forbid_mutations")]
#[case::subscriptions("subscription")]
#[case::override_subgraph_url("override_subgraph_url")]
#[case::authorization("authorization")]
#[case::authentication("authentication")]
#[case::file_upload("preview_file_uploads")]
#[case::response_cache("response_cache")]
#[case::demand_control("demand_control")]
#[case::connectors("connectors")]
#[case::coprocessor("coprocessor")]
#[case::mock_subgraphs("experimental_mock_subgraphs")]
async fn test_optional_plugin_added_alongside_oss_plugins(#[case] plugin: &str) {
    let router_config = Configuration::builder()
        .apollo_plugin("forbid_mutations", plugin_config("forbid_mutations"))
        .apollo_plugin(
            "override_subgraph_url",
            plugin_config("override_subgraph_url"),
        )
        .apollo_plugin("connectors", plugin_config("connectors"))
        .apollo_plugin("response_cache", plugin_config("response_cache"))
        .apollo_plugin(plugin, plugin_config(plugin))
        .build()
        .unwrap();

    let plugins = build_pipeline_plugins(router_config).await;

    assert!(
        plugins.contains_key(&format!("apollo.{plugin}")),
        "Plugin {plugin} should have been added"
    );
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { plugins.contains_key(*plugin) })
    );
    assert!(
        OSS_PLUGINS
            .iter()
            .all(|plugin| { plugins.contains_key(*plugin) })
    );
}
