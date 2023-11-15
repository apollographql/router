use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceExt;

use crate::json_ext::Object;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::SubgraphResponse;

static REDACTED_ERROR_MESSAGE: &str = "Subgraph errors redacted";

register_plugin!("apollo", "include_subgraph_errors", IncludeSubgraphErrors);

/// Configuration for exposing errors that originate from subgraphs
#[derive(Clone, Debug, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct Config {
    /// Include errors from all subgraphs
    all: bool,

    /// Include errors from specific subgraphs
    subgraphs: HashMap<String, bool>,
}

struct IncludeSubgraphErrors {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for IncludeSubgraphErrors {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(IncludeSubgraphErrors {
            config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        // Search for subgraph in our configured subgraph map.
        // If we can't find it, use the "all" value
        if !*self.config.subgraphs.get(name).unwrap_or(&self.config.all) {
            let sub_name_response = name.to_string();
            let sub_name_error = name.to_string();
            return service
                .map_response(move |mut response: SubgraphResponse| {
                    if !response.response.body().errors.is_empty() {
                        tracing::info!("redacted subgraph({sub_name_response}) errors");
                        for error in response.response.body_mut().errors.iter_mut() {
                            error.message = REDACTED_ERROR_MESSAGE.to_string();
                            error.extensions = Object::default();
                        }
                    }
                    response
                })
                // _error to stop clippy complaining about unused assignments...
                .map_err(move |mut _error: BoxError| {
                    // Create a redacted error to replace whatever error we have
                    tracing::info!("redacted subgraph({sub_name_error}) error");
                    _error = Box::new(crate::error::FetchError::SubrequestHttpError {
                        status_code: None,
                        service: "redacted".to_string(),
                        reason: "redacted".to_string(),
                    });
                    _error
                })
                .boxed();
        }
        service
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use bytes::Bytes;
    use once_cell::sync::Lazy;
    use serde_json::Value as jValue;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::Service;

    use super::*;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraph;
    use crate::plugin::DynPlugin;
    use crate::query_planner::BridgeQueryPlanner;
    use crate::router_factory::create_plugins;
    use crate::services::layers::persisted_queries::PersistedQueryLayer;
    use crate::services::layers::query_analysis::QueryAnalysisLayer;
    use crate::services::router;
    use crate::services::router_service::RouterCreator;
    use crate::services::HasSchema;
    use crate::services::PluggableSupergraphServiceBuilder;
    use crate::services::SupergraphRequest;
    use crate::Configuration;

    static UNREDACTED_PRODUCT_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(r#"{"data":{"topProducts":null},"errors":[{"message":"couldn't find mock for query {\"query\":\"query ErrorTopProducts__products__0($first:Int){topProducts(first:$first){__typename upc name}}\",\"operationName\":\"ErrorTopProducts__products__0\",\"variables\":{\"first\":2}}","extensions":{"test":"value","code":"FETCH_ERROR"}}]}"#.as_bytes())
    });

    static REDACTED_PRODUCT_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(
            r#"{"data":{"topProducts":null},"errors":[{"message":"Subgraph errors redacted"}]}"#
                .as_bytes(),
        )
    });

    static REDACTED_ACCOUNT_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(
            r#"{"data":null,"errors":[{"message":"Subgraph errors redacted"}]}"#.as_bytes(),
        )
    });

    static EXPECTED_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#.as_bytes())
    });

    static VALID_QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

    static ERROR_PRODUCT_QUERY: &str = r#"query ErrorTopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

    static ERROR_ACCOUNT_QUERY: &str = r#"query Query { me { name }}"#;

    async fn execute_router_test(
        query: &str,
        body: &Bytes,
        mut router_service: router::BoxService,
    ) {
        let request = SupergraphRequest::fake_builder()
            .query(query.to_string())
            .variable("first", 2usize)
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let response = router_service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*body, response);
    }

    async fn build_mock_router(plugin: Box<dyn DynPlugin>) -> router::BoxService {
        let mut extensions = Object::new();
        extensions.insert("test", Value::String(ByteString::from("value")));

        let account_mocks = vec![
            (
                r#"{"query":"query TopProducts__accounts__3($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}","operationName":"TopProducts__accounts__3","variables":{"representations":[{"__typename":"User","id":"1"},{"__typename":"User","id":"2"}]}}"#,
                r#"{"data":{"_entities":[{"name":"Ada Lovelace"},{"name":"Alan Turing"}]}}"#
            )
        ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();
        let account_service = MockSubgraph::new(account_mocks);

        let review_mocks = vec![
            (
                r#"{"query":"query TopProducts__reviews__1($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{id product{__typename upc}author{__typename id}}}}}","operationName":"TopProducts__reviews__1","variables":{"representations":[{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"2"}]}}"#,
                r#"{"data":{"_entities":[{"reviews":[{"id":"1","product":{"__typename":"Product","upc":"1"},"author":{"__typename":"User","id":"1"}},{"id":"4","product":{"__typename":"Product","upc":"1"},"author":{"__typename":"User","id":"2"}}]},{"reviews":[{"id":"2","product":{"__typename":"Product","upc":"2"},"author":{"__typename":"User","id":"1"}}]}]}}"#
            )
            ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();
        let review_service = MockSubgraph::new(review_mocks);

        let product_mocks = vec![
            (
                r#"{"query":"query TopProducts__products__0($first:Int){topProducts(first:$first){__typename upc name}}","operationName":"TopProducts__products__0","variables":{"first":2}}"#,
                r#"{"data":{"topProducts":[{"__typename":"Product","upc":"1","name":"Table"},{"__typename":"Product","upc":"2","name":"Couch"}]}}"#
            ),
            (
                r#"{"query":"query TopProducts__products__2($representations:[_Any!]!){_entities(representations:$representations){...on Product{name}}}","operationName":"TopProducts__products__2","variables":{"representations":[{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"2"}]}}"#,
                r#"{"data":{"_entities":[{"name":"Table"},{"name":"Couch"}]}}"#
            )
            ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();

        let product_service = MockSubgraph::new(product_mocks).with_extensions(extensions);

        let schema =
            include_str!("../../../apollo-router-benchmarks/benches/fixtures/supergraph.graphql");
        let planner = BridgeQueryPlanner::new(schema.to_string(), Default::default())
            .await
            .unwrap();
        let schema = planner.schema();

        let mut builder = PluggableSupergraphServiceBuilder::new(planner);

        let plugins = create_plugins(&Configuration::default(), &schema, None)
            .await
            .unwrap();

        for (name, plugin) in plugins.into_iter() {
            builder = builder.with_dyn_plugin(name, plugin);
        }

        let builder = builder
            .with_dyn_plugin("apollo.include_subgraph_errors".to_string(), plugin)
            .with_subgraph_service("accounts", account_service.clone())
            .with_subgraph_service("reviews", review_service.clone())
            .with_subgraph_service("products", product_service.clone());

        let supergraph_creator = builder.build().await.expect("should build");

        RouterCreator::new(
            QueryAnalysisLayer::new(supergraph_creator.schema(), Default::default()).await,
            Arc::new(PersistedQueryLayer::new(&Default::default()).await.unwrap()),
            Arc::new(supergraph_creator),
            Arc::new(Configuration::default()),
        )
        .await
        .unwrap()
        .make()
        .boxed()
    }

    async fn get_redacting_plugin(config: &jValue) -> Box<dyn DynPlugin> {
        // Build a redacting plugin
        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.include_subgraph_errors")
            .expect("Plugin not found")
            .create_instance_without_schema(config)
            .await
            .expect("Plugin not created")
    }

    #[tokio::test]
    async fn it_returns_valid_response() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": false })).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(VALID_QUERY, &EXPECTED_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_redacts_all_subgraphs_explicit_redact() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": false })).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_redacts_all_subgraphs_implicit_redact() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({})).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_subgraphs_explicit_allow() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": true })).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_implicit_redact_product_explict_allow_for_product_query() {
        // Build a redacting plugin
        let plugin =
            get_redacting_plugin(&serde_json::json!({ "subgraphs": {"products": true }})).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_implicit_redact_product_explict_allow_for_review_query() {
        // Build a redacting plugin
        let plugin =
            get_redacting_plugin(&serde_json::json!({ "subgraphs": {"reviews": true }})).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_explicit_allow_review_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"reviews": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_explicit_allow_product_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"products": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_explicit_allow_account_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"accounts": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_explicit_allow_account_explict_redact_for_account_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"accounts": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_ACCOUNT_QUERY, &REDACTED_ACCOUNT_RESPONSE, router).await;
    }
}
