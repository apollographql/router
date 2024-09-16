use insta::assert_yaml_snapshot;
use serde_json::json;
use tower::BoxError;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::Mock;
use wiremock::ResponseTemplate;

use crate::integration::common::graph_os_enabled;
use crate::integration::IntegrationTest;

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
    router.assert_log_contains("INTERNAL_SERVER_ERROR").await;
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
        .and(body_partial_json(json!({"version":1,"stage":"RouterRequest","control":"continue","body":"{\"query\":\"query {topProducts{name}}\",\"variables\":{}}","method":"POST"})))
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
    let (_trace_id, response) = router.execute_huge_query().await;
    assert_eq!(response.status(), 413);
    assert_yaml_snapshot!(response.text().await?);

    router.graceful_shutdown().await;
    Ok(())
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
