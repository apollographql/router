//! Traffic shaping plugin
//!
//! Currently includes:
//! * Query deduplication
//!
//! Future functionality:
//! * APQ (already written, but config needs to be moved here)
//! * Caching
//! * Rate limiting
//!

mod deduplication;

use std::collections::HashMap;

use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugins::traffic_shaping::deduplication::QueryDeduplicationLayer;
use crate::register_plugin;
use crate::services::subgraph_service::Compression;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Shaping {
    /// Enable query deduplication
    query_deduplication: Option<bool>,
    /// Enable compression for subgraphs (available compressions are deflate, br, gzip)
    compression: Option<Compression>,
}

impl Shaping {
    fn merge(&self, fallback: Option<&Shaping>) -> Shaping {
        match fallback {
            None => self.clone(),
            Some(fallback) => Shaping {
                query_deduplication: self.query_deduplication.or(fallback.query_deduplication),
                compression: self.compression.or(fallback.compression),
            },
        }
    }
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    all: Option<Shaping>,
    #[serde(default)]
    subgraphs: HashMap<String, Shaping>,
    /// Enable variable deduplication optimization when sending requests to subgraphs (https://github.com/apollographql/router/issues/87)
    variables_deduplication: Option<bool>,
}

struct TrafficShaping {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for TrafficShaping {
    type Config = Config;

    async fn new(config: Self::Config) -> Result<Self, BoxError> {
        Ok(Self { config })
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        // Either we have the subgraph config and we merge it with the all config, or we just have the all config or we have nothing.
        let all_config = self.config.all.as_ref();
        let subgraph_config = self.config.subgraphs.get(name);
        let final_config = Self::merge_config(all_config, subgraph_config);

        if let Some(config) = final_config {
            ServiceBuilder::new()
                .option_layer(config.query_deduplication.unwrap_or_default().then(|| {
                    // Buffer is required because dedup layer requires a clone service.
                    ServiceBuilder::new()
                        .layer(QueryDeduplicationLayer::default())
                        .buffered()
                }))
                .service(service)
                .map_request(move |mut req: SubgraphRequest| {
                    if let Some(compression) = config.compression {
                        let compression_header_val = HeaderValue::from_str(&compression.to_string()).expect("compression is manually implemented and already have the right values; qed");
                        req.subgraph_request.headers_mut().insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip, br, deflate"));
                        req.subgraph_request.headers_mut().insert(CONTENT_ENCODING, compression_header_val);
                    }

                    req
                })
                .boxed()
        } else {
            service
        }
    }

    fn query_planning_service(
        &self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        if matches!(self.config.variables_deduplication, Some(true)) {
            service
                .map_request(|mut req: QueryPlannerRequest| {
                    req.query_plan_options.enable_variable_deduplication = true;
                    req
                })
                .boxed()
        } else {
            service
        }
    }
}

impl TrafficShaping {
    fn merge_config(
        all_config: Option<&Shaping>,
        subgraph_config: Option<&Shaping>,
    ) -> Option<Shaping> {
        let merged_subgraph_config = subgraph_config.map(|c| c.merge(all_config));
        merged_subgraph_config.or_else(|| all_config.cloned())
    }
}

register_plugin!("apollo", "traffic_shaping", TrafficShaping);

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use once_cell::sync::Lazy;
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

    static EXPECTED_RESPONSE: Lazy<Response> = Lazy::new(|| {
        serde_json::from_str(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#).unwrap()
    });

    static VALID_QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

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

    async fn build_mock_router_with_variable_dedup_optimization(
        plugin: Box<dyn DynPlugin>,
    ) -> BoxCloneService<RouterRequest, RouterResponse, BoxError> {
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

        let schema: Arc<Schema> = Arc::new(
            include_str!(
                "../../../../apollo-router-benchmarks/benches/fixtures/supergraph.graphql"
            )
            .parse()
            .unwrap(),
        );

        let builder = PluggableRouterServiceBuilder::new(schema.clone());

        let builder = builder
            .with_dyn_plugin("apollo.traffic_shaping".to_string(), plugin)
            .with_subgraph_service("accounts", account_service.clone())
            .with_subgraph_service("reviews", review_service.clone())
            .with_subgraph_service("products", product_service.clone());

        let router = builder.build().await.expect("should build").test_service();

        router
    }

    async fn get_traffic_shaping_plugin(config: &serde_json::Value) -> Box<dyn DynPlugin> {
        // Build a redacting plugin
        crate::plugin::plugins()
            .get("apollo.traffic_shaping")
            .expect("Plugin not found")
            .create_instance(config)
            .await
            .expect("Plugin not created")
    }

    #[tokio::test]
    async fn it_returns_valid_response_for_deduplicated_variables() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        variables_deduplication: true
        "#,
        )
        .unwrap();
        // Build a redacting plugin
        let plugin = get_traffic_shaping_plugin(&config).await;
        let router = build_mock_router_with_variable_dedup_optimization(plugin).await;
        execute_router_test(VALID_QUERY, &*EXPECTED_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_add_correct_headers_for_compression() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        subgraphs:
            test:
                compression: gzip
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;
        let request = SubgraphRequest::fake_builder().build();

        let test_service = MockSubgraph::new(HashMap::new()).map_request(|req: SubgraphRequest| {
            assert_eq!(
                req.subgraph_request
                    .headers()
                    .get(&CONTENT_ENCODING)
                    .unwrap(),
                HeaderValue::from_static("gzip")
            );
            assert_eq!(
                req.subgraph_request
                    .headers()
                    .get(&ACCEPT_ENCODING)
                    .unwrap(),
                HeaderValue::from_static("gzip, br, deflate")
            );

            req
        });

        let _response = plugin
            .subgraph_service("test", test_service.boxed())
            .oneshot(request)
            .await
            .unwrap();
    }

    #[test]
    fn test_merge_config() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          query_deduplication: true
        subgraphs: 
          products:
            query_deduplication: false
        "#,
        )
        .unwrap();

        assert_eq!(TrafficShaping::merge_config(None, None), None);
        assert_eq!(
            TrafficShaping::merge_config(config.all.as_ref(), None),
            config.all
        );
        assert_eq!(
            TrafficShaping::merge_config(config.all.as_ref(), config.subgraphs.get("products"))
                .as_ref(),
            config.subgraphs.get("products")
        );

        assert_eq!(
            TrafficShaping::merge_config(None, config.subgraphs.get("products")).as_ref(),
            config.subgraphs.get("products")
        );
    }
}
