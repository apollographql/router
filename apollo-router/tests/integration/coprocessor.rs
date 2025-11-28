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
