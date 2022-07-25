use std::collections::HashMap;

use once_cell::sync::Lazy;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceExt;

use crate::error::Error as SubgraphError;
use crate::plugin::Plugin;
use crate::register_plugin;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

#[allow(clippy::field_reassign_with_default)]
static REDACTED_ERROR_MESSAGE: Lazy<Vec<SubgraphError>> = Lazy::new(|| {
    let mut error: SubgraphError = Default::default();

    error.message = "Subgraph errors redacted".to_string();

    vec![error]
});

register_plugin!(
    "experimental",
    "include_subgraph_errors",
    IncludeSubgraphErrors
);

#[derive(Clone, Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Config {
    #[serde(default)]
    all: bool,
    #[serde(default)]
    subgraphs: HashMap<String, bool>,
}

struct IncludeSubgraphErrors {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for IncludeSubgraphErrors {
    type Config = Config;

    async fn new(config: Self::Config) -> Result<Self, BoxError> {
        Ok(IncludeSubgraphErrors { config })
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        // Search for subgraph in our configured subgraph map.
        // If we can't find it, use the "all" value
        if !*self.config.subgraphs.get(name).unwrap_or(&self.config.all) {
            let sub_name_response = name.to_string();
            let sub_name_error = name.to_string();
            return service
                .map_response(move |mut response: SubgraphResponse| {
                    if !response.response.body().errors.is_empty() {
                        tracing::info!("redacted subgraph({sub_name_response}) errors");
                        response.response.body_mut().errors = REDACTED_ERROR_MESSAGE.clone();
                    }
                    response
                })
                // _error to stop clippy complaining about unused assignments...
                .map_err(move |mut _error: BoxError| {
                    // Create a redacted error to replace whatever error we have
                    tracing::info!("redacted subgraph({sub_name_error}) error");
                    _error = Box::new(crate::error::FetchError::SubrequestHttpError {
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
    use serde_json::Value as jValue;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::util::BoxCloneService;
    use tower::Service;

    use super::*;
    use crate::graphql::Response;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraph;
    use crate::plugin::DynPlugin;
    use crate::PluggableRouterServiceBuilder;
    use crate::RouterRequest;
    use crate::RouterResponse;
    use crate::Schema;

    static UNREDACTED_PRODUCT_RESPONSE: Lazy<Response> = Lazy::new(|| {
        serde_json::from_str(r#"{"data": {"topProducts":null}, "errors":[{"message": "couldn't find mock for query", "locations": [], "path": null, "extensions": { "test": "value" }}]}"#).unwrap()
    });

    static REDACTED_PRODUCT_RESPONSE: Lazy<Response> = Lazy::new(|| {
        serde_json::from_str(r#"{"data": {"topProducts":null}, "errors":[{"message": "Subgraph errors redacted", "locations": [], "path": null, "extensions": {}}]}"#).unwrap()
    });

    static REDACTED_ACCOUNT_RESPONSE: Lazy<Response> = Lazy::new(|| {
        Response::from_bytes("account", Bytes::from_static(r#"{
                "data": null,
                "errors":[{"message": "Subgraph errors redacted", "locations": [], "path": null, "extensions": {}}]}"#.as_bytes())
    ).unwrap()
    });

    static EXPECTED_RESPONSE: Lazy<Response> = Lazy::new(|| {
        serde_json::from_str(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#).unwrap()
    });

    static VALID_QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

    static ERROR_PRODUCT_QUERY: &str = r#"query ErrorTopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

    static ERROR_ACCOUNT_QUERY: &str = r#"query Query { me { name }}"#;

    async fn execute_router_test(
        query: &str,
        body: &Response,
        mut router_service: BoxCloneService<RouterRequest, RouterResponse, BoxError>,
    ) {
        let request = RouterRequest::fake_builder()
            .query(query.to_string())
            .variable("first", 2usize)
            .build()
            .expect("expecting valid request");

        let response = router_service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        assert_eq!(response, *body);
    }

    async fn build_mock_router(
        plugin: Box<dyn DynPlugin>,
    ) -> BoxCloneService<RouterRequest, RouterResponse, BoxError> {
        let mut extensions = Object::new();
        extensions.insert("test", Value::String(ByteString::from("value")));

        let account_mocks = vec![
            (
                r#"{"query":"query TopProducts__accounts__3($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}","operationName":"TopProducts__accounts__3","variables":{"representations":[{"__typename":"User","id":"1"},{"__typename":"User","id":"2"},{"__typename":"User","id":"1"}]}}"#,
                r#"{"data":{"_entities":[{"name":"Ada Lovelace"},{"name":"Alan Turing"},{"name":"Ada Lovelace"}]}}"#
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
                r#"{"query":"query TopProducts__products__2($representations:[_Any!]!){_entities(representations:$representations){...on Product{name}}}","operationName":"TopProducts__products__2","variables":{"representations":[{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"2"}]}}"#,
                r#"{"data":{"_entities":[{"name":"Table"},{"name":"Table"},{"name":"Couch"}]}}"#
            )
            ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();

        let product_service = MockSubgraph::new(product_mocks).with_extensions(extensions);

        let schema: Arc<Schema> = Arc::new(
            include_str!("../../../apollo-router-benchmarks/benches/fixtures/supergraph.graphql")
                .parse()
                .unwrap(),
        );

        let builder = PluggableRouterServiceBuilder::new(schema.clone());
        let builder = builder
            .with_dyn_plugin("experimental.include_subgraph_errors".to_string(), plugin)
            .with_subgraph_service("accounts", account_service.clone())
            .with_subgraph_service("reviews", review_service.clone())
            .with_subgraph_service("products", product_service.clone());

        let router = builder.build().await.expect("should build").test_service();

        router
    }

    async fn get_redacting_plugin(config: &jValue) -> Box<dyn DynPlugin> {
        // Build a redacting plugin
        crate::plugin::plugins()
            .get("experimental.include_subgraph_errors")
            .expect("Plugin not found")
            .create_instance(config)
            .await
            .expect("Plugin not created")
    }

    #[tokio::test]
    async fn it_returns_valid_response() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": false })).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(VALID_QUERY, &*EXPECTED_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_redacts_all_subgraphs_explicit_redact() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": false })).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_redacts_all_subgraphs_implicit_redact() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({})).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_subgraphs_explicit_allow() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": true })).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_implicit_redact_product_explict_allow_for_product_query() {
        // Build a redacting plugin
        let plugin =
            get_redacting_plugin(&serde_json::json!({ "subgraphs": {"products": true }})).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_implicit_redact_product_explict_allow_for_review_query() {
        // Build a redacting plugin
        let plugin =
            get_redacting_plugin(&serde_json::json!({ "subgraphs": {"reviews": true }})).await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_explicit_allow_review_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"reviews": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_explicit_allow_product_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"products": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_explicit_allow_account_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"accounts": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &*UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_explicit_allow_account_explict_redact_for_account_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"accounts": false }}),
        )
        .await;
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_ACCOUNT_QUERY, &*REDACTED_ACCOUNT_RESPONSE, router).await;
    }
}
