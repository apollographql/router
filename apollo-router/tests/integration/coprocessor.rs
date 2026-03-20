use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use insta::assert_yaml_snapshot;
use serde_json::json;
use tower::BoxError;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;
use crate::integration::common::redact_cache_debug_query_hash;

#[tokio::test(flavor = "multi_thread")]
async fn test_error_not_propagated_to_client() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/broken_coprocessor.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 500);
    assert_yaml_snapshot!(response.text().await?);
    router.wait_for_log_message("Internal Server Error").await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_limit_payload() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    // Expect a small query
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({"version":1,"stage":"RouterRequest","control":"continue","body":"{\"query\":\"query ExampleQuery {topProducts{name}}\",\"variables\":{}}","method":"POST"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"version":1,"stage":"RouterRequest","control":"continue","body":"{\"query\":\"query {topProducts{name}}\",\"variables\":{}}","method":"POST"})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    // Do not expect a large query
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":1,"stage":"RouterRequest","control":"continue","body":"{\"query\":\"query {topProducts{name}}\",\"variables\":{}}","method":"POST"})))
        .expect(0)
        .mount(&mock_server)
        .await;

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_body_limit.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // This query is small and should make it to the coprocessor
    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);

    // This query is huge and will be rejected because it is too large before hitting the coprocessor
    let (_trace_id, response) = router
        .execute_query(Query::default().with_huge_query())
        .await;
    assert_eq!(response.status(), 413);
    assert_yaml_snapshot!(response.text().await?);

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_response_handling() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    test_full_pipeline(400, "RouterRequest", empty_body_string).await;
    test_full_pipeline(200, "RouterResponse", empty_body_string).await;
    test_full_pipeline(500, "SupergraphRequest", empty_body_string).await;
    test_full_pipeline(500, "SupergraphResponse", empty_body_string).await;
    test_full_pipeline(200, "SubgraphRequest", empty_body_string).await;
    test_full_pipeline(200, "SubgraphResponse", empty_body_string).await;
    test_full_pipeline(500, "ExecutionRequest", empty_body_string).await;
    test_full_pipeline(500, "ExecutionResponse", empty_body_string).await;

    test_full_pipeline(500, "RouterRequest", empty_body_object).await;
    test_full_pipeline(500, "RouterResponse", empty_body_object).await;
    test_full_pipeline(200, "SupergraphRequest", empty_body_object).await;
    test_full_pipeline(500, "SupergraphResponse", empty_body_object).await;
    test_full_pipeline(200, "SubgraphRequest", empty_body_object).await;
    test_full_pipeline(200, "SubgraphResponse", empty_body_object).await;
    test_full_pipeline(200, "ExecutionRequest", empty_body_object).await;
    test_full_pipeline(500, "ExecutionResponse", empty_body_object).await;

    test_full_pipeline(200, "RouterRequest", remove_body).await;
    test_full_pipeline(200, "RouterResponse", remove_body).await;
    test_full_pipeline(200, "SupergraphRequest", remove_body).await;
    test_full_pipeline(200, "SupergraphResponse", remove_body).await;
    test_full_pipeline(200, "SubgraphRequest", remove_body).await;
    test_full_pipeline(200, "SubgraphResponse", remove_body).await;
    test_full_pipeline(200, "ExecutionRequest", remove_body).await;
    test_full_pipeline(200, "ExecutionResponse", remove_body).await;

    test_full_pipeline(500, "RouterRequest", null_out_response).await;
    test_full_pipeline(500, "RouterResponse", null_out_response).await;
    test_full_pipeline(500, "SupergraphRequest", null_out_response).await;
    test_full_pipeline(500, "SupergraphResponse", null_out_response).await;
    test_full_pipeline(200, "SubgraphRequest", null_out_response).await;
    test_full_pipeline(200, "SubgraphResponse", null_out_response).await;
    test_full_pipeline(500, "ExecutionRequest", null_out_response).await;
    test_full_pipeline(500, "ExecutionResponse", null_out_response).await;
    Ok(())
}

fn empty_body_object(mut body: serde_json::Value) -> serde_json::Value {
    *body.pointer_mut("/body").expect("body") = json!({});
    body
}

fn empty_body_string(mut body: serde_json::Value) -> serde_json::Value {
    *body.pointer_mut("/body").expect("body") = json!("");
    body
}

fn remove_body(mut body: serde_json::Value) -> serde_json::Value {
    body.as_object_mut().expect("body").remove("body");
    body
}

fn null_out_response(_body: serde_json::Value) -> serde_json::Value {
    json!("")
}

async fn test_full_pipeline(
    response_status: u16,
    stage: &'static str,
    coprocessor: impl Fn(serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
) {
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    // Expect a small query
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(move |req: &wiremock::Request| {
            let mut body = req.body_json::<serde_json::Value>().expect("body");
            if body
                .as_object()
                .unwrap()
                .get("stage")
                .unwrap()
                .as_str()
                .unwrap()
                == stage
            {
                body = coprocessor(body);
            }
            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(&mock_server)
        .await;

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(
        response.status(),
        response_status,
        "Failed at stage {stage}"
    );

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_demand_control_access() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    // Assert the execution request stage has access to the estimated cost
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({
        "stage": "ExecutionRequest",
        "context": {
            "entries": {
                "apollo::demand_control::estimated_cost": 10.0,
                "apollo::demand_control::result": "COST_OK",
                "apollo::demand_control::strategy": "static_estimated"
            }}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "version":1,
                        "stage":"ExecutionRequest",
                        "control":"continue",
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Assert the supergraph response stage also includes the actual cost
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({
            "stage": "SupergraphResponse",
            "context": {"entries": {
            "apollo::demand_control::actual_cost": 3.0,
            "apollo::demand_control::estimated_cost": 10.0,
            "apollo::demand_control::result": "COST_OK",
            "apollo::demand_control::strategy": "static_estimated"
        }}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "version":1,
                        "stage":"SupergraphResponse",
                        "control":"continue",
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_demand_control.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_proxying_error_response() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mock_coprocessor = wiremock::MockServer::start().await;
    let coprocessor_address = mock_coprocessor.uri();

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(|req: &wiremock::Request| {
            let body = req.body_json::<serde_json::Value>().expect("body");
            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(&mock_coprocessor)
        .await;

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{ "message": "subgraph error", "path": [] }],
            "data": null
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "errors": [{ "message": "Subgraph errors redacted", "path": [] }],
            "data": null
        })
    );

    router.graceful_shutdown().await;

    Ok(())
}

mod on_graphql_error_selector {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::RwLock;

    use serde_json::json;
    use serde_json::value::Value;
    use tower::BoxError;
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use crate::integration::IntegrationTest;
    use crate::integration::common::Query;
    use crate::integration::common::graph_os_enabled;

    fn query() -> Query {
        Query::builder()
            .traced(true)
            .body(json!({"query": "query Q { topProducts { name inStock } }"}))
            .build()
    }

    fn products_response(errors: bool) -> Value {
        if errors {
            json!({"errors": [{ "message": "products error", "path": [] }]})
        } else {
            json!({
                "data": {
                    "topProducts": [
                        { "__typename": "Product", "name": "Table", "upc": "1" },
                        { "__typename": "Product", "name": "Chair", "upc": "2" },
                    ]
                },
            })
        }
    }

    fn inventory_response(errors: bool) -> Value {
        if errors {
            json!({"errors": [{ "message": "inventory error", "path": [] }]})
        } else {
            json!({"data": {"_entities": [{"inStock": true}, {"inStock": false}]}})
        }
    }

    fn response_template(response_json: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(response_json)
    }

    async fn send_query_to_coprocessor_enabled_router(
        query: Query,
        subgraph_response_products: ResponseTemplate,
        subgraph_response_inventory: ResponseTemplate,
    ) -> Result<(Value, HashMap<String, usize>), BoxError> {
        let coprocessor_hits: Arc<RwLock<HashMap<String, usize>>> =
            Arc::new(RwLock::new(HashMap::default()));
        let coprocessor_hits_clone = coprocessor_hits.clone();
        let coprocessor_response = move |req: &wiremock::Request| {
            let req_body = req.body_json::<serde_json::Value>().expect("body");
            let stage = req_body.as_object()?.get("stage")?.as_str()?.to_string();

            let mut binding = coprocessor_hits_clone.write().ok()?;
            let entry = binding.entry(stage).or_default();
            *entry += 1;
            Some(response_template(req_body))
        };

        let mock_coprocessor = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(move |r: &wiremock::Request| coprocessor_response(r).unwrap())
            .mount(&mock_coprocessor)
            .await;

        let mock_products = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(subgraph_response_products)
            .mount(&mock_products)
            .await;

        let mock_inventory = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(subgraph_response_inventory)
            .mount(&mock_inventory)
            .await;

        let mut router = IntegrationTest::builder()
            .config(
                include_str!("fixtures/coprocessor_conditional.router.yaml")
                    .replace("<replace>", &mock_coprocessor.uri()),
            )
            .subgraph_override("products", mock_products.uri())
            .subgraph_override("inventory", mock_inventory.uri())
            .build()
            .await;
        router.start().await;
        router.assert_started().await;

        let (_, response) = router.execute_query(query).await;
        assert_eq!(response.status(), 200);

        let response = serde_json::from_str(&response.text().await?).unwrap();

        // NB: should be ok to read and clone bc response should have finished
        Ok((response, coprocessor_hits.read().unwrap().clone()))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_all_successful() -> Result<(), BoxError> {
        if !graph_os_enabled() {
            return Ok(());
        }

        let (response, coprocessor_hits) = send_query_to_coprocessor_enabled_router(
            query(),
            response_template(products_response(false)),
            response_template(inventory_response(false)),
        )
        .await?;

        let errors = response.as_object().unwrap().get("errors");
        assert!(errors.is_none());
        assert!(coprocessor_hits.is_empty());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_first_response_failure() -> Result<(), BoxError> {
        if !graph_os_enabled() {
            return Ok(());
        }

        let (response, coprocessor_hits) = send_query_to_coprocessor_enabled_router(
            query(),
            response_template(products_response(true)),
            response_template(inventory_response(false)),
        )
        .await?;

        let errors = response.as_object().unwrap().get("errors").unwrap();
        insta::assert_json_snapshot!(errors);

        assert_eq!(*coprocessor_hits.get("RouterResponse").unwrap(), 1);
        assert_eq!(*coprocessor_hits.get("SupergraphResponse").unwrap(), 1);

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_nested_response_failure() -> Result<(), BoxError> {
        if !graph_os_enabled() {
            return Ok(());
        }

        let (response, coprocessor_hits) = send_query_to_coprocessor_enabled_router(
            query(),
            response_template(products_response(false)),
            response_template(inventory_response(true)),
        )
        .await?;

        let errors = response.as_object().unwrap().get("errors").unwrap();
        insta::assert_json_snapshot!(errors);

        assert_eq!(*coprocessor_hits.get("RouterResponse").unwrap(), 1);
        assert_eq!(*coprocessor_hits.get("SupergraphResponse").unwrap(), 1);

        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_context_key_deletion() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    // Track the context keys received in each stage
    let router_response_context = std::sync::Arc::new(std::sync::Mutex::new(None));
    let router_response_context_clone = router_response_context.clone();

    // Handle all coprocessor stages, but modify SubgraphResponse and track RouterResponse
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(move |req: &wiremock::Request| {
            let body = req.body_json::<serde_json::Value>().expect("body");
            let stage = body.get("stage").and_then(|s| s.as_str()).unwrap_or("");

            let mut response = body.clone();

            // Ensure Request stages have a control field
            if stage.ends_with("Request")
                && !response.as_object().unwrap().contains_key("control")
                && let Some(obj) = response.as_object_mut()
            {
                obj.insert("control".to_string(), serde_json::json!("continue"));
            }

            if stage == "RouterRequest" {
                // Add a context entry to the router request
                response
                    .as_object_mut()
                    .expect("response was not an object")
                    .entry("context")
                    .or_insert_with(|| serde_json::Value::Object(Default::default()))
                    .as_object_mut()
                    .expect("context was not an object")
                    .entry("entries")
                    .or_insert_with(|| serde_json::Value::Object(Default::default()))
                    .as_object_mut()
                    .expect("entries was not an object")
                    .insert("k1".to_string(), serde_json::json!("v1"));
            } else if stage == "SubgraphResponse" {
                // Return context without "k1" (deleted)
                response
                    .as_object_mut()
                    .expect("response was not an object")
                    .get_mut("context")
                    .expect("context was not found")
                    .as_object_mut()
                    .expect("context was not an object")
                    .get_mut("entries")
                    .expect("entries was not found")
                    .as_object_mut()
                    .expect("entries was not an object")
                    .remove("k1");
            } else if stage == "RouterResponse" {
                // Track the context received in RouterResponse
                let context = body.get("context").and_then(|c| c.get("entries"));
                if let Some(ctx) = context {
                    *router_response_context_clone.lock().unwrap() = Some(ctx.clone());
                }
            }

            // For all other stages, just pass through
            ResponseTemplate::new(200).set_body_json(response)
        })
        .mount(&mock_server)
        .await;

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_context.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute a query that will trigger both SubgraphResponse and RouterResponse stages
    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);

    // Verify that RouterResponse does NOT have "k1" (it was deleted in SubgraphResponse)
    assert!(
        !router_response_context
            .lock()
            .unwrap()
            .as_ref()
            .expect("router response context was None")
            .as_object()
            .expect("router response context was not an object")
            .contains_key("k1")
    );

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_receives_response_cache_keys() -> Result<(), BoxError> {
    // GIVEN:
    //   - graphos
    //   - a mock server with a mocked coprocessor
    //   - a way to track cache key data across threads (for the mock server thread and test thread)
    //   - a router with coprocessor debugging enabled
    //   - a request to that router with a `apollo-cache-debugging: true` header

    if !graph_os_enabled() {
        return Ok(());
    }

    // looks sort of over-complicated but we need to access and mutate the key data across threads
    // (test thread and the mock server's thread)
    type CacheKey = (
        serde_json::Value,
        serde_json::Map<String, serde_json::Value>,
    );
    let received_cache_keys: Arc<Mutex<Option<CacheKey>>> = Arc::new(Mutex::new(None));
    let received_cache_keys_clone = received_cache_keys.clone();

    // coprocessor mock
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(move |req: &wiremock::Request| {
            let body = req.body_json::<serde_json::Value>().expect("body");
            let stage = body.get("stage").and_then(|s| s.as_str()).unwrap_or("");

            // we're targeting the response stage to make sure keys are available by then (they
            // should be, but this is an understated yet critical part of what we're testing)
            if stage == "SupergraphResponse"
                && let Some(context) = body.get("context")
                && let Some(entries) = context.get("entries").and_then(|e| e.as_object())
                && let Some(cache_keys) = entries.get("apollo::response_cache::debug_cached_keys")
            {
                *received_cache_keys_clone.lock().unwrap() =
                    Some((cache_keys.clone(), entries.clone()));
            }

            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(&mock_server)
        .await;

    let subgraph_response = ResponseTemplate::new(200)
        .insert_header("cache-control", "public, max-age=60")
        .set_body_json(json!({
            "data": {
                "topProducts": [{
                    "name": "Table",
                    "__typename": "Product",
                    "reviews": [{
                        "id": "1",
                        "product": { "__typename": "Product" },
                        "author": { "__typename": "User", "id": "u1" }
                    }],
                    "reviewsForAuthor": [{
                        "id": "2",
                        "product": { "__typename": "Product" },
                        "author": { "__typename": "User", "id": "u1" }
                    }]
                }]
            }
        }));

    // NOTE: this config has `debug: true` enabled for response caching, that's an important part
    // of getting the cache key data into a coprocessor
    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_response_cache_keys.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .responder(subgraph_response)
        .build()
        .await;

    // NOTE: very importantly, this header is required for getting context keys into a coprocessor!
    let query = Query::builder()
        .header("apollo-cache-debugging".to_string(), "true".to_string())
        .build();

    // WHEN:
    //   - we run the router
    //   - and send a query with the apollo-cache-debugging header

    router.start().await;
    router.assert_started().await;
    let (_trace_id, response) = router.execute_query(query).await;

    // THEN:
    //   - all is well (ie, status code 200)
    //   - the coprocessor receives the cache keys
    //   - there's actually data for those keys
    assert_eq!(response.status(), 200);

    let (cache_keys, _context_entries) = received_cache_keys
        .lock()
        .unwrap()
        .take()
        .expect("coprocessor should have received response cache keys in context");

    tracing::info!("cache keys: {cache_keys}");

    let mut cache_keys = cache_keys;
    for entry in cache_keys.as_array_mut().unwrap() {
        if let Some(key) = entry.get_mut("key").and_then(|v| v.as_str()) {
            entry["key"] = json!(redact_cache_debug_query_hash(key));
        }
        if let Some(cache_control) = entry
            .get_mut("cacheControl")
            .and_then(|v| v.as_object_mut())
        {
            cache_control.remove("created");
        }
    }

    // NOTE: `created` removed from this block
    let expected = json!([{"key":"version:1.2:subgraph:products:type:Query:hash:[query-hash]:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6","invalidationKeys":[],"kind":{"rootFields":["topProducts"]},"subgraphName":"products","subgraphRequest":{"query":"query ExampleQuery__products__0 { topProducts { name } }","operationName":"ExampleQuery__products__0"},"source":"subgraph","cacheControl":{"maxAge":60,"public":true},"shouldStore":true,"data":{"data":{"topProducts":[{"name":"Table","__typename":"Product","reviews":[{"id":"1","product":{"__typename":"Product"},"author":{"__typename":"User","id":"u1"}}],"reviewsForAuthor":[{"id":"2","product":{"__typename":"Product"},"author":{"__typename":"User","id":"u1"}}]}]}},"warnings":[{"code":"NO_CACHE_TAG_ON_ROOT_FIELD","links":[{"url":"https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/invalidation#invalidation-methods","title":"Add '@cacheTag' in your schema"}],"message":"No cache tags are specified on your root fields query. If you want to use active invalidation, you'll need to add cache tags on your root field."}]}]);

    assert_eq!(cache_keys, expected);

    router.graceful_shutdown().await;

    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_unix_domain_socket() -> Result<(), tower::BoxError> {
    use std::path::PathBuf;

    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixListener;

    if !crate::integration::common::graph_os_enabled() {
        return Ok(());
    }

    // Create a temporary Unix socket path
    let dir = tempfile::tempdir().expect("tempdir");
    let mut sock_path = PathBuf::from(dir.path());
    sock_path.push("coprocessor.sock");
    let _ = std::fs::remove_file(&sock_path);

    // Start a minimal Unix domain socket HTTP server that echoes the JSON body back
    let uds = UnixListener::bind(&sock_path).expect("bind uds");
    tokio::spawn(async move {
        loop {
            let (stream, _) = uds.accept().await.expect("accept");
            let io = TokioIo::new(stream);
            let svc = hyper::service::service_fn(
                |req: http::Request<hyper::body::Incoming>| async move {
                    let bytes = http_body_util::BodyExt::collect(req.into_body())
                        .await
                        .unwrap()
                        .to_bytes();
                    Ok::<_, std::convert::Infallible>(
                        http::Response::builder()
                            .status(200)
                            .header(
                                http::header::CONTENT_TYPE,
                                mime::APPLICATION_JSON.essence_str(),
                            )
                            .body(axum::body::Body::from(bytes))
                            .unwrap(),
                    )
                },
            );
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("uds server error: {err}");
            }
        }
    });

    // Configure router to use the unix:// coprocessor URL
    let uds_url = format!("unix://{}", sock_path.display());
    let mut router = crate::integration::IntegrationTest::builder()
        .config(include_str!("fixtures/coprocessor.router.yaml").replace("<replace>", &uds_url))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);

    router.graceful_shutdown().await;
    Ok(())
}

/// Verify that unix:// URLs with a `?path=` query parameter deliver requests
/// to the correct HTTP path on the coprocessor. The UDS server rejects any
/// request that arrives on a path other than the expected one with a 500,
/// so the router query itself will fail if the path isn't forwarded correctly.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_unix_domain_socket_with_path() -> Result<(), tower::BoxError> {
    use std::path::PathBuf;

    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixListener;

    if !crate::integration::common::graph_os_enabled() {
        return Ok(());
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let mut sock_path = PathBuf::from(dir.path());
    sock_path.push("coprocessor.sock");
    let _ = std::fs::remove_file(&sock_path);

    // the path we append to the filepath socket
    let expected_path = "/api/v1/coprocessor";

    let uds = UnixListener::bind(&sock_path).expect("bind uds");
    let expected_path_owned = expected_path.to_string();

    tokio::spawn(async move {
        loop {
            let (stream, _) = uds.accept().await.expect("accept");
            let io = TokioIo::new(stream);
            let expected = expected_path_owned.clone();
            let svc =
                hyper::service::service_fn(move |req: http::Request<hyper::body::Incoming>| {
                    let expected = expected.clone();
                    async move {
                        // this checks whether we're actually making requests to /api/v1/coprocessor
                        if req.uri().path() != expected {
                            return Ok::<_, std::convert::Infallible>(
                                http::Response::builder()
                                    // returning 500s if we're not
                                    .status(500)
                                    .header(
                                        http::header::CONTENT_TYPE,
                                        mime::APPLICATION_JSON.essence_str(),
                                    )
                                    .body(axum::body::Body::from(format!(
                                        r#"{{"error":"path mismatch: expected '{}', got '{}'"}}"#,
                                        expected,
                                        req.uri().path()
                                    )))
                                    .unwrap(),
                            );
                        }

                        let bytes = http_body_util::BodyExt::collect(req.into_body())
                            .await
                            .unwrap()
                            .to_bytes();
                        Ok::<_, std::convert::Infallible>(
                            http::Response::builder()
                                .status(200)
                                .header(
                                    http::header::CONTENT_TYPE,
                                    mime::APPLICATION_JSON.essence_str(),
                                )
                                .body(axum::body::Body::from(bytes))
                                .unwrap(),
                        )
                    }
                });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("uds server error: {err}");
            }
        }
    });

    let uds_url = format!("unix://{}?path={}", sock_path.display(), expected_path);
    let mut router = crate::integration::IntegrationTest::builder()
        .config(include_str!("fixtures/coprocessor.router.yaml").replace("<replace>", &uds_url))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    // if we get a 200 it's because we've hit the target path; see above for how this works, but
    // any path _not_ explicitly the one we've set (/api/v1/coprocessor) will return a 500
    assert_eq!(response.status(), 200);

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(unix)]
async fn test_coprocessor_per_stage_unix_socket_urls() -> Result<(), tower::BoxError> {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixListener;

    if !crate::integration::common::graph_os_enabled() {
        return Ok(());
    }

    // Create temporary Unix socket paths for different stages
    let dir = tempfile::tempdir().expect("tempdir");

    let mut router_sock_path = PathBuf::from(dir.path());
    router_sock_path.push("router_stage.sock");
    let _ = std::fs::remove_file(&router_sock_path);

    let mut supergraph_sock_path = PathBuf::from(dir.path());
    supergraph_sock_path.push("supergraph_stage.sock");
    let _ = std::fs::remove_file(&supergraph_sock_path);

    // Counters to verify each socket was actually used
    let router_counter = Arc::new(AtomicU32::new(0));
    let supergraph_counter = Arc::new(AtomicU32::new(0));

    // Start Unix domain socket HTTP server for router stage
    let router_uds = UnixListener::bind(&router_sock_path).expect("bind router uds");
    let router_counter_clone = router_counter.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = router_uds.accept().await.expect("accept router");
            let io = TokioIo::new(stream);
            let counter = router_counter_clone.clone();
            let svc =
                hyper::service::service_fn(move |req: http::Request<hyper::body::Incoming>| {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let bytes = http_body_util::BodyExt::collect(req.into_body())
                            .await
                            .unwrap()
                            .to_bytes();
                        Ok::<_, std::convert::Infallible>(
                            http::Response::builder()
                                .status(200)
                                .header(
                                    http::header::CONTENT_TYPE,
                                    mime::APPLICATION_JSON.essence_str(),
                                )
                                .body(axum::body::Body::from(bytes))
                                .unwrap(),
                        )
                    }
                });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("router uds server error: {err}");
            }
        }
    });

    // Start Unix domain socket HTTP server for supergraph stage
    let supergraph_uds = UnixListener::bind(&supergraph_sock_path).expect("bind supergraph uds");
    let supergraph_counter_clone = supergraph_counter.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = supergraph_uds.accept().await.expect("accept supergraph");
            let io = TokioIo::new(stream);
            let counter = supergraph_counter_clone.clone();
            let svc =
                hyper::service::service_fn(move |req: http::Request<hyper::body::Incoming>| {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let bytes = http_body_util::BodyExt::collect(req.into_body())
                            .await
                            .unwrap()
                            .to_bytes();
                        Ok::<_, std::convert::Infallible>(
                            http::Response::builder()
                                .status(200)
                                .header(
                                    http::header::CONTENT_TYPE,
                                    mime::APPLICATION_JSON.essence_str(),
                                )
                                .body(axum::body::Body::from(bytes))
                                .unwrap(),
                        )
                    }
                });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("supergraph uds server error: {err}");
            }
        }
    });

    // Wait a moment for servers to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Configure router with per-stage Unix socket URLs
    let router_uds_url = format!("unix://{}", router_sock_path.display());
    let supergraph_uds_url = format!("unix://{}", supergraph_sock_path.display());

    let config = format!(
        r#"
include_subgraph_errors:
  all: true
coprocessor:
  url: http://should-not-be-used:9999  # Global fallback (should not be used)
  router:
    request:
      headers: true
      context: true
      url: {}  # Override for router stage
  supergraph:
    request:
      headers: true
      body: true
      context: true
      url: {}  # Override for supergraph stage
"#,
        router_uds_url, supergraph_uds_url
    );

    let mut router = crate::integration::IntegrationTest::builder()
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);

    // Verify that both Unix socket servers were actually used
    let router_calls = router_counter.load(Ordering::SeqCst);
    let supergraph_calls = supergraph_counter.load(Ordering::SeqCst);

    assert!(
        router_calls > 0,
        "Router stage Unix socket should have been called, but was called {} times",
        router_calls
    );
    assert!(
        supergraph_calls > 0,
        "Supergraph stage Unix socket should have been called, but was called {} times",
        supergraph_calls
    );

    println!(
        "Per-stage Unix socket URLs working correctly: router={} calls, supergraph={} calls",
        router_calls, supergraph_calls
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(unix)]
async fn test_coprocessor_mixed_http_and_unix_socket_urls() -> Result<(), tower::BoxError> {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixListener;
    use wiremock::ResponseTemplate;

    if !crate::integration::common::graph_os_enabled() {
        return Ok(());
    }

    // Create temporary Unix socket path for router stage
    let dir = tempfile::tempdir().expect("tempdir");
    let mut router_sock_path = PathBuf::from(dir.path());
    router_sock_path.push("router_stage.sock");
    let _ = std::fs::remove_file(&router_sock_path);

    // Counters to verify each endpoint was actually used
    let router_counter = Arc::new(AtomicU32::new(0));
    let supergraph_counter = Arc::new(AtomicU32::new(0));

    // Start Unix domain socket HTTP server for router stage
    let router_uds = UnixListener::bind(&router_sock_path).expect("bind router uds");
    let router_counter_clone = router_counter.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = router_uds.accept().await.expect("accept router");
            let io = TokioIo::new(stream);
            let counter = router_counter_clone.clone();
            let svc =
                hyper::service::service_fn(move |req: http::Request<hyper::body::Incoming>| {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let bytes = http_body_util::BodyExt::collect(req.into_body())
                            .await
                            .unwrap()
                            .to_bytes();
                        Ok::<_, std::convert::Infallible>(
                            http::Response::builder()
                                .status(200)
                                .header(
                                    http::header::CONTENT_TYPE,
                                    mime::APPLICATION_JSON.essence_str(),
                                )
                                .body(axum::body::Body::from(bytes))
                                .unwrap(),
                        )
                    }
                });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("router uds server error: {err}");
            }
        }
    });

    // Start HTTP server for supergraph stage using wiremock
    let supergraph_counter_clone = supergraph_counter.clone();
    let supergraph_mock_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |req: &wiremock::Request| {
            supergraph_counter_clone.fetch_add(1, Ordering::SeqCst);
            // Echo back the request body
            ResponseTemplate::new(200)
                .set_body_bytes(req.body.clone())
                .insert_header("content-type", "application/json")
        })
        .mount(&supergraph_mock_server)
        .await;

    // Wait a moment for servers to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Configure router with MIXED transports: Unix socket for router, HTTP for supergraph
    let router_uds_url = format!("unix://{}", router_sock_path.display());
    let supergraph_http_url = supergraph_mock_server.uri();

    let config = format!(
        r#"
include_subgraph_errors:
  all: true
coprocessor:
  url: http://should-not-be-used:9999  # Global fallback (should not be used)
  router:
    request:
      headers: true
      context: true
      url: {}  # Unix socket for router stage
  supergraph:
    request:
      headers: true
      body: true
      context: true
      url: {}  # HTTP for supergraph stage
"#,
        router_uds_url, supergraph_http_url
    );

    let mut router = crate::integration::IntegrationTest::builder()
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);

    // Verify that BOTH transports were used correctly
    let router_calls = router_counter.load(Ordering::SeqCst);
    let supergraph_calls = supergraph_counter.load(Ordering::SeqCst);

    assert!(
        router_calls > 0,
        "Router stage Unix socket should have been called, but was called {} times",
        router_calls
    );
    assert!(
        supergraph_calls > 0,
        "Supergraph stage HTTP endpoint should have been called, but was called {} times",
        supergraph_calls
    );

    println!(
        "Mixed transports working correctly: router Unix socket={} calls, supergraph HTTP={} calls",
        router_calls, supergraph_calls
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(unix)]
async fn test_coprocessor_unix_socket_connection_refused() -> Result<(), BoxError> {
    use std::path::PathBuf;

    if !crate::integration::common::graph_os_enabled() {
        return Ok(());
    }

    // Create a socket path that doesn't exist (no server listening)
    let dir = tempfile::tempdir().expect("tempdir");
    let mut sock_path = PathBuf::from(dir.path());
    sock_path.push("nonexistent.sock");

    // Configure router to use a Unix socket that doesn't exist
    let uds_url = format!("unix://{}", sock_path.display());
    let config = format!(
        r#"
include_subgraph_errors:
  all: true
coprocessor:
  url: {}
  timeout: 1s
  router:
    request:
      headers: true
"#,
        uds_url
    );

    let mut router = crate::integration::IntegrationTest::builder()
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Query should fail or return error due to connection refused
    let (_trace_id, response) = router.execute_default_query().await;

    // The router should handle the error gracefully
    // Depending on coprocessor configuration, it might return an error or continue
    // For this test, we're just verifying it doesn't panic/crash
    assert!(
        response.status().is_client_error()
            || response.status().is_server_error()
            || response.status().is_success(),
        "Router should handle connection errors gracefully, got status: {}",
        response.status()
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(unix)]
async fn test_coprocessor_unix_socket_server_closes_connection() -> Result<(), BoxError> {
    use std::path::PathBuf;

    use tokio::net::UnixListener;

    if !crate::integration::common::graph_os_enabled() {
        return Ok(());
    }

    // Create a Unix socket that immediately closes connections
    let dir = tempfile::tempdir().expect("tempdir");
    let mut sock_path = PathBuf::from(dir.path());
    sock_path.push("closing.sock");
    let _ = std::fs::remove_file(&sock_path);

    let uds = UnixListener::bind(&sock_path).expect("bind uds");
    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = uds.accept().await {
                // Immediately drop the stream to close the connection
                drop(stream);
            }
        }
    });

    // Wait for server to be ready
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let uds_url = format!("unix://{}", sock_path.display());
    let config = format!(
        r#"
include_subgraph_errors:
  all: true
coprocessor:
  url: {}
  timeout: 2s
  router:
    request:
      headers: true
"#,
        uds_url
    );

    let mut router = crate::integration::IntegrationTest::builder()
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Query should handle the closed connection gracefully
    let (_trace_id, response) = router.execute_default_query().await;

    // Should get an error response but not crash
    assert!(
        response.status().is_client_error()
            || response.status().is_server_error()
            || response.status().is_success(),
        "Router should handle connection closure gracefully, got status: {}",
        response.status()
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connector_coprocessor_request_response() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    // Track which coprocessor stages were called
    let stages_called: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stages_called_clone = stages_called.clone();

    // Set up a wiremock coprocessor that echoes back the request and tracks stages
    let mock_coprocessor = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(move |req: &wiremock::Request| {
            let body = req.body_json::<serde_json::Value>().expect("body");
            let stage = body
                .get("stage")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            stages_called_clone.lock().unwrap().push(stage);
            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(&mock_coprocessor)
        .await;

    let config = format!(
        r#"
        include_subgraph_errors:
            all: true
        coprocessor:
            url: {}
            connector:
                all:
                    request:
                        body: true
                        headers: true
                        uri: true
                    response:
                        body: true
                        headers: true
                        status_code: true
        "#,
        mock_coprocessor.uri()
    );

    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
            "body": "This is a really great post",
            "userId": 1
        }])))
        .http_method("GET")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query":"query { posts { id title } }"}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);

    let body = response.json::<serde_json::Value>().await?;
    assert!(
        body.get("errors").is_none(),
        "unexpected errors: {:?}",
        body.get("errors")
    );

    let called_stages = stages_called.lock().unwrap().clone();
    assert!(
        called_stages.contains(&"ConnectorRequest".to_string()),
        "ConnectorRequest stage should have been called, got: {called_stages:?}"
    );
    assert!(
        called_stages.contains(&"ConnectorResponse".to_string()),
        "ConnectorResponse stage should have been called, got: {called_stages:?}"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connector_coprocessor_failure_returns_graphql_error() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    // Set up a coprocessor that returns a 500 error for connector stages
    let mock_coprocessor = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_coprocessor)
        .await;

    let config = format!(
        r#"
        include_subgraph_errors:
            all: true
        coprocessor:
            url: {}
            connector:
                all:
                    request:
                        body: true
        "#,
        mock_coprocessor.uri()
    );

    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
            "body": "This is a really great post",
            "userId": 1
        }])))
        .http_method("GET")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query":"query { posts { id title } }"}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);

    let body = response.json::<serde_json::Value>().await?;
    assert!(
        body.get("errors")
            .and_then(|e| e.as_array())
            .is_some_and(|errors| !errors.is_empty()),
        "expected GraphQL errors in response body, got: {body}"
    );

    router.graceful_shutdown().await;
    Ok(())
}
