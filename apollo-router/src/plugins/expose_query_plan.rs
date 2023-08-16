use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use http::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceExt as TowerServiceExt;

use crate::layers::ServiceExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::execution;
use crate::services::supergraph;

const EXPOSE_QUERY_PLAN_HEADER_NAME: &str = "Apollo-Expose-Query-Plan";
const ENABLE_EXPOSE_QUERY_PLAN_ENV: &str = "APOLLO_EXPOSE_QUERY_PLAN";
const QUERY_PLAN_CONTEXT_KEY: &str = "experimental::expose_query_plan.plan";
const FORMATTED_QUERY_PLAN_CONTEXT_KEY: &str = "experimental::expose_query_plan.formatted_plan";
const ENABLED_CONTEXT_KEY: &str = "experimental::expose_query_plan.enabled";

#[derive(Debug, Clone)]
struct ExposeQueryPlan {
    enabled: bool,
}

/// Expose query plan
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExposeQueryPlanConfig(
    /// Enabled
    bool,
);

#[async_trait::async_trait]
impl Plugin for ExposeQueryPlan {
    type Config = ExposeQueryPlanConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(ExposeQueryPlan {
            enabled: init.config.0
                || std::env::var(ENABLE_EXPOSE_QUERY_PLAN_ENV).as_deref() == Ok("true"),
        })
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        service
            .map_request(move |req: execution::Request| {
                if req
                    .context
                    .get::<_, bool>(ENABLED_CONTEXT_KEY)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    req.context
                        .insert(QUERY_PLAN_CONTEXT_KEY, req.query_plan.root.clone())
                        .unwrap();
                    req.context
                        .insert(
                            FORMATTED_QUERY_PLAN_CONTEXT_KEY,
                            req.query_plan.formatted_query_plan.clone(),
                        )
                        .unwrap();
                }

                req
            })
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let conf_enabled = self.enabled;
        service
            .map_future_with_request_data(move |req: &supergraph::Request| {
                let is_enabled = conf_enabled && req.supergraph_request.headers().get(EXPOSE_QUERY_PLAN_HEADER_NAME) == Some(&HeaderValue::from_static("true"));
                if is_enabled {
                    req.context.insert(ENABLED_CONTEXT_KEY, true).unwrap();
                }

                is_enabled
            }, move | is_enabled: bool, f| async move {
                let mut res: supergraph::ServiceResult = f.await;

                res = match res {
                    Ok(mut res) => {
                        if is_enabled {
                            let (parts, stream) = res.response.into_parts();
                            let (mut first, rest) = stream.into_future().await;

                            if let Some(first) = &mut first {
                                if let Some(plan) =
                                    res.context.get_json_value(QUERY_PLAN_CONTEXT_KEY)
                                {
                                    first
                                        .extensions
                                        .insert("apolloQueryPlan", json!({ "object": { "kind": "QueryPlan", "node": plan }, "text": res.context.get_json_value(FORMATTED_QUERY_PLAN_CONTEXT_KEY) }));
                                }
                            }
                            res.response = http::Response::from_parts(
                                parts,
                                once(ready(first.unwrap_or_default())).chain(rest).boxed(),
                            );
                        }

                        Ok(res)
                    }
                    Err(err) => Err(err),
                };

                res
            })
            .boxed()
    }
}

register_plugin!("experimental", "expose_query_plan", ExposeQueryPlan);

#[cfg(test)]
mod tests {
    use once_cell::sync::Lazy;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::Service;

    use super::*;
    use crate::graphql::Response;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraph;
    use crate::MockedSubgraphs;

    static EXPECTED_RESPONSE_WITH_QUERY_PLAN: Lazy<Response> = Lazy::new(|| {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/expected_response_with_queryplan.json"
        ))
        .unwrap()
    });
    static EXPECTED_RESPONSE_WITHOUT_QUERY_PLAN: Lazy<Response> = Lazy::new(|| {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/expected_response_without_queryplan.json"
        ))
        .unwrap()
    });

    static VALID_QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

    async fn build_mock_supergraph(config: serde_json::Value) -> supergraph::BoxCloneService {
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

        let subgraphs = MockedSubgraphs(
            [
                ("accounts", account_service),
                ("reviews", review_service),
                ("products", product_service),
            ]
            .into_iter()
            .collect(),
        );

        crate::TestHarness::builder()
            .schema(include_str!(
                "../../../apollo-router-benchmarks/benches/fixtures/supergraph.graphql"
            ))
            .extra_plugin(subgraphs)
            .configuration_json(config)
            .unwrap()
            .build_supergraph()
            .await
            .unwrap()
    }

    async fn execute_supergraph_test(
        query: &str,
        body: &Response,
        mut supergraph_service: supergraph::BoxCloneService,
    ) {
        let request = supergraph::Request::fake_builder()
            .query(query.to_string())
            .variable("first", 2usize)
            .header(EXPOSE_QUERY_PLAN_HEADER_NAME, "true")
            .build()
            .expect("expecting valid request");

        let response = supergraph_service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            serde_json::to_string(body).unwrap()
        );
    }

    #[tokio::test]
    async fn it_expose_query_plan() {
        execute_supergraph_test(
            VALID_QUERY,
            &EXPECTED_RESPONSE_WITH_QUERY_PLAN,
            build_mock_supergraph(serde_json::json! {{
                "plugins": {
                    "experimental.expose_query_plan": true
                }
            }})
            .await,
        )
        .await;
        // let's try that again
        execute_supergraph_test(
            VALID_QUERY,
            &EXPECTED_RESPONSE_WITH_QUERY_PLAN,
            build_mock_supergraph(serde_json::json! {{
                "plugins": {
                    "experimental.expose_query_plan": true
                }
            }})
            .await,
        )
        .await;
    }

    #[tokio::test]
    async fn it_doesnt_expose_query_plan() {
        let supergraph = build_mock_supergraph(serde_json::json! {{
            "plugins": {
                "experimental.expose_query_plan": false
            }
        }})
        .await;
        execute_supergraph_test(
            VALID_QUERY,
            &EXPECTED_RESPONSE_WITHOUT_QUERY_PLAN,
            supergraph,
        )
        .await;
    }
}
