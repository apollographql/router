use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

fn error_responder() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": { "topProducts": null },
        "errors": [{
            "message": "something went wrong",
            "path": ["topProducts"]
        }]
    }))
}

fn success_responder() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": {
            "topProducts": [
                {"name": "Table"},
                {"name": "Couch"},
                {"name": "Chair"}
            ]
        }
    }))
}

fn default_query() -> Query {
    Query::builder()
        .body(json!({"query": "{ topProducts { name } }", "variables": {}}))
        .build()
}

fn extract_error_codes(body: &serde_json::Value) -> Vec<String> {
    body.get("errors")
        .and_then(|v| v.as_array())
        .map(|errs| {
            errs.iter()
                .filter_map(|e| {
                    e.get("extensions")
                        .and_then(|ext| ext.get("code"))
                        .and_then(|c| c.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_circuit_trips_after_threshold() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: true
                error_threshold: 2
                window: 60s
                recovery_timeout: 300s
                mode: enforce
            "#,
        )
        .responder(error_responder())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // First two requests hit the subgraph and record errors
    for i in 0..2 {
        let (_trace_id, response) = router.execute_query(default_query()).await;
        let body: serde_json::Value = response.json().await?;
        let codes = extract_error_codes(&body);
        assert!(
            !codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
            "request {i} should pass through to subgraph, got: {codes:?}"
        );
    }

    // Third request should be rejected by the circuit breaker
    let (_trace_id, response) = router.execute_query(default_query()).await;
    let body: serde_json::Value = response.json().await?;
    let codes = extract_error_codes(&body);
    assert!(
        codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
        "expected CIRCUIT_BREAKER_OPEN after threshold, got: {codes:?}"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_measure_mode_does_not_reject() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: true
                error_threshold: 1
                window: 60s
                recovery_timeout: 300s
                mode: measure
            "#,
        )
        .responder(error_responder())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Trip the threshold
    let (_trace_id, _response) = router.execute_query(default_query()).await;

    // Even after exceeding the threshold, measure mode should not reject
    for _ in 0..3 {
        let (_trace_id, response) = router.execute_query(default_query()).await;
        let body: serde_json::Value = response.json().await?;
        let codes = extract_error_codes(&body);
        assert!(
            !codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
            "measure mode should never produce CIRCUIT_BREAKER_OPEN"
        );
    }

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_disabled_circuit_breaking_passes_through() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: false
            "#,
        )
        .responder(error_responder())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..5 {
        let (_trace_id, response) = router.execute_query(default_query()).await;
        let body: serde_json::Value = response.json().await?;
        let codes = extract_error_codes(&body);
        assert!(
            !codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
            "disabled circuit breaking should never reject"
        );
    }

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_successful_responses_do_not_trip_circuit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: true
                error_threshold: 2
                window: 60s
                recovery_timeout: 300s
                mode: enforce
            "#,
        )
        .responder(success_responder())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..10 {
        let (_trace_id, response) = router.execute_query(default_query()).await;
        let body: serde_json::Value = response.json().await?;
        let codes = extract_error_codes(&body);
        assert!(
            !codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
            "successful responses should never trip the circuit"
        );
        assert!(body.get("data").is_some(), "expected data in response");
    }

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_circuit_breaker_error_includes_extension_fields() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: true
                error_threshold: 1
                window: 60s
                recovery_timeout: 300s
                mode: enforce
            "#,
        )
        .responder(error_responder())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Trip the circuit
    let (_trace_id, _response) = router.execute_query(default_query()).await;

    // Next request should be rejected with well-formed error
    let (_trace_id, response) = router.execute_query(default_query()).await;
    let body: serde_json::Value = response.json().await?;
    let error = &body["errors"][0];
    assert_eq!(
        error["extensions"]["code"].as_str(),
        Some("CIRCUIT_BREAKER_OPEN"),
    );
    assert!(
        error["extensions"]["service"].is_null(),
        "error should not leak subgraph name in 'service' extension: {error:?}"
    );
    assert_eq!(
        error["message"].as_str(),
        Some("Circuit breaker is open"),
        "error message should be generic: {error:?}"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_http_500_from_subgraph_trips_circuit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: true
                error_threshold: 2
                window: 60s
                recovery_timeout: 300s
                mode: enforce
            "#,
        )
        .responder(
            ResponseTemplate::new(500)
                .set_body_json(json!({"errors": [{"message": "Internal Server Error"}]})),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, _response) = router.execute_query(default_query()).await;
    let (_trace_id, _response) = router.execute_query(default_query()).await;

    let (_trace_id, response) = router.execute_query(default_query()).await;
    let body: serde_json::Value = response.json().await?;
    let codes = extract_error_codes(&body);
    assert!(
        codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
        "expected CIRCUIT_BREAKER_OPEN after HTTP 500 errors, got: {codes:?} body: {body:?}"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_config_reload_enables_circuit_breaking() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: false
            "#,
        )
        .responder(error_responder())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // With circuit breaking disabled, errors should pass through
    for _ in 0..5 {
        let (_trace_id, response) = router.execute_query(default_query()).await;
        let body: serde_json::Value = response.json().await?;
        let codes = extract_error_codes(&body);
        assert!(
            !codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
            "disabled circuit breaking should not reject"
        );
    }

    // Enable circuit breaking via config reload
    router
        .update_config(
            r#"
            include_subgraph_errors:
              all: true
            circuit_breaking:
              all:
                enabled: true
                error_threshold: 1
                window: 60s
                recovery_timeout: 300s
                mode: enforce
            "#,
        )
        .await;
    router.assert_reloaded().await;

    // First request records the error
    let (_trace_id, _response) = router.execute_query(default_query()).await;

    // Second request should be rejected by the newly-enabled circuit breaker
    let (_trace_id, response) = router.execute_query(default_query()).await;
    let body: serde_json::Value = response.json().await?;
    let codes = extract_error_codes(&body);
    assert!(
        codes.contains(&"CIRCUIT_BREAKER_OPEN".to_string()),
        "expected CIRCUIT_BREAKER_OPEN after config reload enabled circuit breaking, got: {codes:?}"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_starts_with_full_circuit_breaking_config() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            circuit_breaking:
              all:
                enabled: true
                error_threshold: 5
                window: 30s
                recovery_timeout: 60s
                half_open_max_requests: 1
                mode: enforce
              subgraphs:
                products:
                  enabled: true
                  error_threshold: 3
                  mode: measure
              connector:
                all:
                  enabled: true
                  error_threshold: 3
                  window: 30s
                  recovery_timeout: 60s
                  mode: enforce
                sources:
                  "connectors.jsonPlaceholder":
                    error_threshold: 10
            "#,
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
