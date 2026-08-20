//! Which plugins a built pipeline instantiates: mandatory and OSS plugins always,
//! optional plugins gated by the license's allowed features.

use std::collections::HashSet;
use std::sync::Arc;

use rstest::rstest;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tower_http::BoxError;

use crate::AllowedFeature;
use crate::configuration::Configuration;
use crate::pipeline::plugins::inject_schema_id;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::router_factory::PipelineFactory;
use crate::router_factory::RouterServiceFactory;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseLimits;
use crate::uplink::license_enforcement::LicenseState;

const MANDATORY_PLUGINS: &[&str] = &[
    "apollo.include_subgraph_errors",
    "apollo.headers",
    "apollo.license_enforcement",
    "apollo.health_check",
    "apollo.traffic_shaping",
    "apollo.limits",
    "apollo.csrf",
    "apollo.fleet_detector",
    "apollo.enhanced_client_awareness",
    "apollo.progressive_override",
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
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    /// The name of the test
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
            Arc::new(LicenseState::default()),
        )
        .await;
    service.map(|_| ())
}

#[tokio::test]
async fn test_yaml_no_extras() {
    let config = Configuration::builder().build().unwrap();
    let service = create_service(config).await;
    assert!(service.is_ok())
}

#[tokio::test]
async fn test_yaml_plugins_always_starts_and_stops() {
    let config: Configuration = serde_yaml::from_str(
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
    let config: Configuration = serde_yaml::from_str(
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
    let config: Configuration = serde_yaml::from_str(
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

#[test]
fn test_inject_schema_id() {
    let mut config = json!({ "apollo": {} });
    inject_schema_id(
        "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8",
        &mut config,
    );
    let config = serde_json::from_value::<crate::plugins::telemetry::config::Conf>(config).unwrap();
    assert_eq!(
        &config.apollo.schema_id,
        "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8"
    );
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

#[tokio::test]
#[rstest]
#[case::empty_allowed_features_set(HashSet::new())]
#[case::nonempty_allowed_features_set(HashSet::from_iter(vec![AllowedFeature::Coprocessors]))]
async fn test_mandatory_plugins_added(#[case] allowed_features: HashSet<AllowedFeature>) {
    /*
     * GIVEN
     *  - a valid license
     *  - a valid config
     *  - a valid schema
     * */
    let license = LicenseState::Licensed {
        limits: Some(LicenseLimits {
            tps: None,
            allowed_features,
        }),
    };

    let router_config = Configuration::builder().build().unwrap();
    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    /*
     * WHEN
     *  - the router factory runs (including the plugin inits gated by the license)
     * */
    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
            Arc::new(license),
        )
        .await
        .unwrap();

    /*
     * THEN
     *  - the mandatory plugins are added
     * */
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
}

#[tokio::test]
#[rstest]
#[case::allowed_features_empty(HashSet::new())]
#[case::allowed_features_nonempty(HashSet::from_iter(vec![
    AllowedFeature::Coprocessors,
    AllowedFeature::DemandControl
]))]
async fn test_oss_plugins_added(#[case] allowed_features: HashSet<AllowedFeature>) {
    /*
     * GIVEN
     *  - a valid license
     *  - a valid config that contains configuration for oss plugins
     *  - a valid schema
     * */
    let license = LicenseState::Licensed {
        limits: Some(LicenseLimits {
            tps: None,
            allowed_features,
        }),
    };

    // Create config for oss plugins
    let forbid_mutations_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config("forbid_mutations")).unwrap();
    let override_subgraph_url_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config("override_subgraph_url"))
            .unwrap();
    let connectors_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config("connectors")).unwrap();

    let router_config = Configuration::builder()
        .apollo_plugin("forbid_mutations", forbid_mutations_config)
        .apollo_plugin("override_subgraph_url", override_subgraph_url_config)
        .apollo_plugin("connectors", connectors_config)
        .build()
        .unwrap();

    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    /*
     * WHEN
     *  - the router factory runs (including the plugin inits gated by the license)
     * */
    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
            Arc::new(license),
        )
        .await
        .unwrap();

    /*
     * THEN
     *  - all oss plugins should have been added
     * */
    assert!(
        OSS_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
}

#[tokio::test]
#[rstest]
#[case::subscripions(
    "subscription",
    HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::Subscriptions]))
]
#[case::authorization(
    "authorization",
    HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions]))
]
#[case::authentication(
    "authentication",
    HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::Authentication, AllowedFeature::Subscriptions]))
]
#[case::response_cache(
    "response_cache",
    HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::ResponseCaching]))
]
#[case::authorization(
    "demand_control",
    HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions, AllowedFeature::DemandControl]))
]
#[case::coprocessor(
    "coprocessor",
    HashSet::from_iter(vec![AllowedFeature::Coprocessors, AllowedFeature::DemandControl]))
]
async fn test_optional_plugin_added_with_restricted_allowed_features(
    #[case] plugin: &str,
    #[case] allowed_features: HashSet<AllowedFeature>,
) {
    /*
     * GIVEN
     *  - a restricted license with allowed feature set containing the given `plugin`
     *  - a valid config including valid config for the given `plugin`
     *  - a valid schema
     * */
    let license = LicenseState::Licensed {
        limits: Some(LicenseLimits {
            tps: None,
            allowed_features,
        }),
    };

    let plugin_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
    dbg!(&plugin_config);
    let router_config = Configuration::builder()
        .apollo_plugin(plugin, plugin_config)
        .build()
        .unwrap();

    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    /*
     * WHEN
     *  - the router factory runs (including the plugin inits gated by the license)
     * */
    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
            Arc::new(license),
        )
        .await
        .unwrap();

    /*
     * THEN
     *  - since the plugin is part of the `allowed_features` set
     *    the plugin should have been added.
     * - mandatory plugins should have been added.
     * */
    assert!(
        service.plugins.contains_key(&format!("apollo.{plugin}")),
        "Plugin {plugin} should have been added"
    );
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
}

#[tokio::test]
#[rstest]
#[case::subscripions(
    "subscription",
    HashSet::from_iter(vec![]))
]
#[case::authorization(
    "authorization",
    HashSet::from_iter(vec![AllowedFeature::Authentication, AllowedFeature::Subscriptions]))
]
#[case::authentication(
    "authentication",
    HashSet::from_iter(vec![AllowedFeature::DemandControl,AllowedFeature::Subscriptions]))
]
#[case::response_cache(
    "response_cache",
    HashSet::from_iter(vec![AllowedFeature::Authentication]))
]
#[case::authorization(
    "demand_control",
    HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions, AllowedFeature::Experimental]))
]
#[case::coprocessor(
    "coprocessor",
    HashSet::from_iter(vec![AllowedFeature::DemandControl]))
]
async fn test_optional_plugin_not_added_with_restricted_allowed_features(
    #[case] plugin: &str,
    #[case] allowed_features: HashSet<AllowedFeature>,
) {
    /*
     * GIVEN
     *  - a restricted license whose allowed feature set does not contain the given `plugin`
     *  - a valid config including valid config for the given `plugin`
     *  - a valid schema
     * */
    let license = LicenseState::Licensed {
        limits: Some(LicenseLimits {
            tps: None,
            allowed_features,
        }),
    };

    let plugin_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
    let router_config = Configuration::builder()
        .apollo_plugin(plugin, plugin_config)
        .build()
        .unwrap();

    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    /*
     * WHEN
     *  - the router factory runs (including the plugin inits gated by the license)
     * */
    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
            Arc::new(license),
        )
        .await
        .unwrap();

    /*
     * THEN
     *  - since the plugin is not part of the `allowed_features` set
     *    the plugin should not have been added.
     * - mandatory plugins should have been added.
     * */
    assert!(
        !service.plugins.contains_key(&format!("apollo.{plugin}")),
        "Plugin {plugin} should not have been added"
    );
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
}

#[tokio::test]
#[rstest]
#[case::mock_subgraphs_non_empty_allowed_features(
    "experimental_mock_subgraphs",
    HashSet::from_iter(vec![AllowedFeature::DemandControl])
)]
#[case::mock_subgraphs_empty_allowed_features(
    "experimental_mock_subgraphs",
    HashSet::from_iter(vec![])
)]
async fn test_optional_plugin_that_does_not_map_to_an_allowed_feature_is_added(
    #[case] plugin: &str,
    #[case] allowed_features: HashSet<AllowedFeature>,
) {
    /*
     * GIVEN
     *  - a valid license
     *  - a valid config including valid config for the optional plugin that does
     *    not map to an allowed feature
     *  - a valid schema
     * */
    let license = LicenseState::Licensed {
        limits: Some(LicenseLimits {
            tps: None,
            allowed_features,
        }),
    };

    let plugin_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
    let router_config = Configuration::builder()
        .apollo_plugin(plugin, plugin_config)
        .build()
        .unwrap();

    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    /*
     * WHEN
     *  - the router factory runs (including the plugin inits gated by the license)
     * */
    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
            Arc::new(license),
        )
        .await
        .unwrap();

    /*
     * THEN
     * - the plugin should be added
     * - mandatory plugins should have been added.
     * - coprocessors and subscritions (both gated features) should not have been added.
     * */
    assert!(
        service.plugins.contains_key(&format!("apollo.{plugin}")),
        "Plugin {plugin} should have been added"
    );
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
    // These gated features should not have been added
    assert!(
        !service.plugins.contains_key("apollo.subscription"),
        "Plugin {plugin} should not have been added"
    );
    assert!(
        !service.plugins.contains_key("apollo.coprocessor"),
        "Plugin {plugin} should not have been added"
    );
}

#[tokio::test]
#[rstest]
// NB: this is temporary behavior and will change once the `allowed_features` claim is in all licenses
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
async fn test_optional_plugin_with_unrestricted_allowed_features(#[case] plugin: &str) {
    /*
     * GIVEN
     *  - a license with unrestricted limits (includes allowing all features)
     *  - a valid config including valid config for the given `plugin`
     *  - a valid schema
     * */
    let license = LicenseState::Licensed {
        limits: Default::default(),
    };

    let plugin_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
    let router_config = Configuration::builder()
        .apollo_plugin(plugin, plugin_config)
        .build()
        .unwrap();

    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    /*
     * WHEN
     *  - the router factory runs (including the plugin inits gated by the license)
     * */
    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
            Arc::new(license),
        )
        .await
        .unwrap();

    /*
     * THEN
     *  - since `allowed_features` is unrestricted plugin should have been added.
     * */
    assert!(
        service.plugins.contains_key(&format!("apollo.{plugin}")),
        "Plugin {plugin} should have been added"
    );
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
}

#[tokio::test]
#[rstest]
// NB: this is temporary behavior and will change once the `allowed_features` claim is in all licenses
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
async fn test_optional_plugin_with_default_license_limits(#[case] plugin: &str) {
    /*
     * GIVEN
     *  - a license with license limits None
     *  - a valid config including valid config for the given `plugin`
     *  - a valid schema
     * */
    let license = LicenseState::Licensed {
        limits: Default::default(),
    };

    // Create config for the given `plugin`
    let plugin_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();

    // Create config for oss plugins
    let forbid_mutations_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config("forbid_mutations")).unwrap();
    let override_subgraph_url_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config("override_subgraph_url"))
            .unwrap();
    let connectors_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config("connectors")).unwrap();
    let response_cache_config =
        serde_yaml::from_str::<serde_json::Value>(get_plugin_config("response_cache")).unwrap();

    let router_config = Configuration::builder()
        .apollo_plugin("forbid_mutations", forbid_mutations_config)
        .apollo_plugin("override_subgraph_url", override_subgraph_url_config)
        .apollo_plugin("connectors", connectors_config)
        .apollo_plugin("response_cache", response_cache_config)
        .apollo_plugin(plugin, plugin_config)
        .build()
        .unwrap();

    let schema = include_str!("../../testdata/supergraph.graphql");
    let schema = Schema::parse(schema, &router_config).unwrap();

    /*
     * WHEN
     *  - the router factory runs (including the plugin inits gated by the license)
     * */
    let is_telemetry_disabled = false;
    let service = PipelineFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(router_config),
            Arc::new(schema),
            None,
            None,
            Arc::new(license),
        )
        .await
        .unwrap();

    /*
     * THEN
     *  // NB: this behavior may change once all licenses have an `allowed_features` claim
     *  - when license limits are None we default to unrestricted allowed features
     *  - the given `plugin` should have been added
     *  - all mandatory plugins should have been added
     *  - all oss plugins in the config should have been added
     * */
    assert!(
        service.plugins.contains_key(&format!("apollo.{plugin}")),
        "Plugin {plugin} should have been added"
    );
    assert!(
        MANDATORY_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
    assert!(
        OSS_PLUGINS
            .iter()
            .all(|plugin| { service.plugins.contains_key(*plugin) })
    );
}
