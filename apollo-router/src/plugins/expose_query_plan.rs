use std::ops::ControlFlow;

use futures::future::ready;
use futures::stream::once;
use futures::FutureExt;
use futures::StreamExt;
use http::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt as TowerServiceExt;

use crate::layers::ServiceBuilderExt;
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

#[derive(Clone, serde::Deserialize, serde::Serialize)]
enum Setting {
    Enabled,
    DryRun,
    Disabled,
}

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
        ServiceBuilder::new()
            .oneshot_checkpoint_async(move |req: execution::Request| {
                async move {
                    let setting = req
                        .context
                        .get::<_, Setting>(ENABLED_CONTEXT_KEY)
                        .ok()
                        .flatten()
                        .unwrap_or(Setting::Disabled);

                    if !matches!(setting, Setting::Disabled) {
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

                    if matches!(setting, Setting::DryRun) {
                        Ok(ControlFlow::Break(
                            execution::Response::error_builder()
                                .errors(vec![])
                                .context(req.context)
                                .build()?,
                        ))
                    } else {
                        Ok(ControlFlow::Continue(req))
                    }
                }
                .boxed()
            })
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let conf_enabled = self.enabled;
        service
            .map_future_with_request_data(move |req: &supergraph::Request| {
                let setting = if conf_enabled {
                    let header = req.supergraph_request.headers().get(EXPOSE_QUERY_PLAN_HEADER_NAME);
                    if header == Some(&HeaderValue::from_static("true")) {
                        Setting::Enabled
                    } else if header == Some(&HeaderValue::from_static("dry-run")) {
                        Setting::DryRun
                    } else {
                        Setting::Disabled
                    }
                } else {
                    Setting::Disabled
                };

                if !matches!(setting, Setting::Disabled) {
                    req.context.insert(ENABLED_CONTEXT_KEY, setting.clone()).unwrap();
                }

                setting
            }, move | setting: Setting, f| async move {
                let mut res: supergraph::ServiceResult = f.await;

                res = match res {
                    Ok(mut res) => {
                        if !matches!(setting, Setting::Disabled) {
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
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::Service;

    use super::*;
    use crate::graphql::Response;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraph;
    use crate::MockedSubgraphs;

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
        mut supergraph_service: supergraph::BoxCloneService,
    ) -> Response {
        let request = supergraph::Request::fake_builder()
            .query(query.to_string())
            .variable("first", 2usize)
            .header(EXPOSE_QUERY_PLAN_HEADER_NAME, "true")
            .build()
            .expect("expecting valid request");

        supergraph_service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
    }

    async fn execute_supergraph_test_dry_run(
        query: &str,
        mut supergraph_service: supergraph::BoxCloneService,
    ) -> Response {
        let request = supergraph::Request::fake_builder()
            .query(query.to_string())
            .variable("first", 2usize)
            .header(EXPOSE_QUERY_PLAN_HEADER_NAME, "dry-run")
            .build()
            .expect("expecting valid request");

        supergraph_service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn it_doesnt_expose_query_plan() {
        let supergraph = build_mock_supergraph(serde_json::json! {{
            "plugins": {
                "experimental.expose_query_plan": false
            }
        }})
        .await;

        // Since we're not exposing the query plan, we expect the extensions (where the query plan
        // would be) to be empty
        //
        // That the extension is empty is important. This lets us assume that when the extension is
        // populated (like in the following tests when we're testing that the query plan is
        // output), it's populated with _only_ the query plan (meaning we won't be experiencing
        // false positives)
        assert!(execute_supergraph_test(VALID_QUERY, supergraph)
            .await
            .extensions
            .is_empty())
    }

    #[tokio::test]
    async fn it_expose_query_plan() {
        let response = execute_supergraph_test(
            VALID_QUERY,
            build_mock_supergraph(serde_json::json! {{
                "plugins": {
                    "experimental.expose_query_plan": true
                }
            }})
            .await,
        )
        .await;

        // Since we're exposing the query plan, the extensions better not be empty! See the test
        // for not exposing query plans to know why the assumption that a non-empty extension means
        // we have a query plan
        assert!(!response.extensions.is_empty());

        // Since this is a full-run (ie, not a dry-run), we should have data
        assert!(response.data.is_some());
    }

    #[tokio::test]
    async fn it_expose_query_plan_without_executing() {
        let response = execute_supergraph_test_dry_run(
            VALID_QUERY,
            build_mock_supergraph(serde_json::json! {{
                "plugins": {
                    "experimental.expose_query_plan": true
                }
            }})
            .await,
        )
        .await;

        // Since we're exposing the query plan, the extensions better not be empty! See the test
        // for not exposing query plans to know why the assumption that a non-empty extension means
        // we have a query plan
        assert!(!response.extensions.is_empty());

        // Since this is a dry-run, we shouldn't have any data
        assert!(response.data.is_none());
    }
}
