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
    router.wait_for_log_message("INTERNAL_SERVER_ERROR").await;
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
                "cost.estimated": 10.0,
                "cost.result": "COST_OK",
                "cost.strategy": "static_estimated"
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
            "cost.actual": 3.0,
            "cost.estimated": 10.0,
            "cost.result": "COST_OK",
            "cost.strategy": "static_estimated"
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
