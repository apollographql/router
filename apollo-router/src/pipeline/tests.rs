use std::sync::Arc;

use indexmap::IndexMap;
use serde_json_bytes::json;

use super::acquire::maybe_bootstrap_telemetry;
use super::*;
use crate::configuration::Configuration;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::Plugins;
use crate::services::SupergraphRequest;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

mod plugin_instantiation;
mod subgraph_apq;

/// Subgraph names in `testdata/supergraph.graphql`, sorted.
const FIXTURE_SUBGRAPHS: [&str; 4] = ["accounts", "inventory", "products", "reviews"];

fn test_configuration() -> Arc<Configuration> {
    Arc::new(Configuration::builder().build().unwrap())
}

fn test_schema(configuration: &Configuration) -> Arc<Schema> {
    Arc::new(
        Schema::parse(
            include_str!("../testdata/supergraph.graphql"),
            configuration,
        )
        .unwrap(),
    )
}

fn sorted_keys<V>(map: &IndexMap<String, V>) -> Vec<&str> {
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

/// A plugin map holding a real traffic-shaping plugin, which
/// [`parse_http_client_inputs`] looks up by name for per-subgraph client config.
async fn plugins_with_traffic_shaping() -> Plugins {
    let traffic_shaping = crate::plugin::plugins()
        .find(|factory| factory.name == APOLLO_TRAFFIC_SHAPING)
        .expect("traffic shaping plugin is registered")
        .create_instance_without_schema(&serde_json::json!({}))
        .await
        .expect("traffic shaping plugin builds from an empty config");
    let mut plugins = Plugins::default();
    plugins.insert(APOLLO_TRAFFIC_SHAPING.to_string(), traffic_shaping);
    plugins
}

/// A request carrying a persisted-query hash and no query string. An enabled APQ
/// expander answers it from its cache; a disabled one rejects it. The two error
/// messages tell the paths apart.
fn hash_only_apq_request() -> SupergraphRequest {
    SupergraphRequest::fake_builder()
        .extension(
            "persistedQuery",
            json!({
                "version": 1,
                "sha256Hash": "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"
            }),
        )
        .build()
        .expect("valid request")
}

#[test]
fn create_query_planner_extracts_a_schema_per_subgraph() {
    let configuration = test_configuration();
    let schema = test_schema(&configuration);

    let (_planner, subgraph_schemas) = create_query_planner(&schema, &configuration).unwrap();

    let mut names: Vec<&str> = subgraph_schemas.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, FIXTURE_SUBGRAPHS);
}

#[tokio::test]
async fn parse_http_client_inputs_covers_every_subgraph() {
    let configuration = test_configuration();
    let schema = test_schema(&configuration);
    let plugins = plugins_with_traffic_shaping().await;

    let client_inputs = parse_http_client_inputs(&plugins, &schema, &configuration).unwrap();

    assert_eq!(sorted_keys(&client_inputs.subgraphs), FIXTURE_SUBGRAPHS);
    assert!(client_inputs.connectors.is_empty());
}

#[tokio::test]
async fn build_http_services_builds_a_client_per_subgraph() {
    let configuration = test_configuration();
    let schema = test_schema(&configuration);
    let plugins = plugins_with_traffic_shaping().await;
    let client_inputs = parse_http_client_inputs(&plugins, &schema, &configuration).unwrap();

    let (subgraph_services, connector_services) =
        build_http_services(client_inputs, &Arc::new(plugins));

    assert_eq!(sorted_keys(&subgraph_services), FIXTURE_SUBGRAPHS);
    assert!(connector_services.is_empty());
}

#[tokio::test]
async fn init_telemetry_returns_no_plugin_on_hot_reload() {
    let configuration = test_configuration();
    let schema = test_schema(&configuration);
    let license = Arc::new(LicenseState::default());

    let plugin = maybe_bootstrap_telemetry(&configuration, &schema, &license, Some(&configuration))
        .await
        .unwrap();

    assert!(plugin.is_none());
}

#[tokio::test]
async fn create_plugins_instantiates_mandatory_plugins() {
    let configuration = test_configuration();
    let schema = test_schema(&configuration);
    let (_planner, subgraph_schemas) =
        create_query_planner(&schema, &configuration).unwrap();

    let plugins = create_plugins(
        &configuration,
        &schema,
        subgraph_schemas,
        None,
        None,
        Arc::new(LicenseState::default()),
        None,
    )
    .await
    .unwrap();

    assert!(plugins.contains_key("apollo.include_subgraph_errors"));
    assert!(plugins.contains_key("apollo.traffic_shaping"));
}

#[tokio::test]
async fn build_query_plan_cache_without_redis_uses_the_configured_capacity() {
    let configuration: Configuration = serde_yaml::from_str(
        r#"
        supergraph:
          query_planning:
            cache:
              in_memory:
                limit: 42
        "#,
    )
    .unwrap();

    let cache = build_query_plan_cache(&configuration, None);

    assert_eq!(cache.in_memory_cache().lock().await.cap().get(), 42);
}

#[tokio::test]
async fn apq_expander_enabled_reports_an_unknown_hash_as_not_found() {
    let configuration = test_configuration();
    assert!(configuration.apq.enabled);

    let expander = build_apq_expander(&configuration, None);
    let mut response = expander
        .supergraph_request(hash_only_apq_request())
        .await
        .expect_err("a cache miss short-circuits the request");

    let graphql = response.next_response().await.expect("one response");
    assert_eq!(graphql.errors[0].message, "PersistedQueryNotFound");
}

#[tokio::test]
async fn apq_expander_disabled_rejects_persisted_query_requests() {
    let mut configuration = Configuration::default();
    configuration.apq.enabled = false;

    let expander = build_apq_expander(&configuration, None);
    let mut response = expander
        .supergraph_request(hash_only_apq_request())
        .await
        .expect_err("persisted queries are rejected when APQ is disabled");

    let graphql = response.next_response().await.expect("one response");
    assert_eq!(graphql.errors[0].message, "PersistedQueryNotSupported");
}

#[tokio::test]
async fn connect_query_plan_redis_is_none_without_redis_config() {
    let configuration = test_configuration();

    let redis = connect_query_plan_redis(&configuration).await.unwrap();

    assert!(redis.is_none());
}

#[tokio::test]
async fn connect_apq_redis_is_none_without_redis_config() {
    let configuration = test_configuration();

    let redis = connect_apq_redis(&configuration).await.unwrap();

    assert!(redis.is_none());
}
