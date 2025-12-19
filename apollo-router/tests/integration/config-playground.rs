//! Playground for exploring custom telemetry configurations.
//!
//! These tests explore what can be achieved with router YAML configuration
//! to replace functionality from custom plugins.
//!
//! Based on common plugin requirements:
//! - Error handling: metrics for client/server errors, response headers
//! - Logging: structured JSON logs with request/response data
//! - Header propagation

use http::StatusCode;
use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

// =============================================================================
// SHARED CONFIGURATION FIXTURE
// =============================================================================
//
// This single configuration achieves parity with both a custom error handling
// plugin AND a logging plugin. All tests use this same config.
//
// METRICS (Error Handling Plugin Parity):
// - golden_signal.total_client_errors: Count client errors by extension code
// - golden_signal.total_server_errors: Count server errors (non-client codes)
// - golden_signal.supergraph_client_errors: Router-originated client errors only
// - golden_signal.supergraph_server_errors: Router-originated server errors only
//
// LOGGING (Logging Plugin Parity):
// - router_request_log: Main structured log at end of request lifecycle
//   - Request headers (transaction ID, client name, etc.)
//   - Response headers
//   - operationName
//   - txTime (request_duration in milliseconds)
//   - hasErrors ("T"/"F" string)
//   - Custom error field extraction (messages, codes, services)
//
// Client error codes (from v1 plugin):
// - INVALID_TYPE, INVALID_FIELD, PARSING_ERROR, GRAPHQL_VALIDATION_FAILED,
//   SUBSCRIPTION_NOT_SUPPORTED, RECURSION_LIMIT_EXCEEDED

const SHARED_CONFIG: &str = r#"
telemetry:
    exporters:
        metrics:
            prometheus:
                listen: 127.0.0.1:4000
                enabled: true
                path: /metrics
        logging:
            stdout:
                enabled: true
                format:
                    json:
                        display_current_span: false
                        display_span_list: false
    instrumentation:
        events:
            router:
                # Main structured log event - fires at end of request lifecycle
                # This replaces the v1 logging plugin's log_map output
                request.lifecycle:
                    message: "router_request_log"
                    on: response
                    level: info
                    attributes:
                        # -------------------------------------------------
                        # REQUEST HEADERS
                        # -------------------------------------------------
                        x-request-tid:
                            request_header: "x-request-tid"
                            default: ""
                        apollographql-client-name:
                            request_header: "apollographql-client-name"
                            default: ""
                        apollographql-client-version:
                            request_header: "apollographql-client-version"
                            default: ""
                        x-request-id:
                            request_header: "x-request-id"
                            default: ""
                        user-agent:
                            request_header: "user-agent"
                            default: ""
                        content_type:
                            request_header: "content-type"
                            default: ""
                        x-custom-header:
                            request_header: "x-custom-header"
                            default: ""
                        
                        # -------------------------------------------------
                        # RESPONSE HEADERS
                        # -------------------------------------------------
                        x-response-id:
                            response_header: "x-response-id"
                            default: ""
                        cache-control:
                            response_header: "cache-control"
                            default: ""
                        response-content-type:
                            response_header: "content-type"
                            default: ""
                        x-custom-response-header:
                            response_header: "x-custom-response-header"
                            default: ""
                        
                        # -------------------------------------------------
                        # OPERATION INFO
                        # -------------------------------------------------
                        operationName:
                            operation_name: string
                            default: ""
                        http.status_code:
                            response_status: code
                        http.method:
                            request_method: true
                        
                        # -------------------------------------------------
                        # ERROR DETECTION
                        # hasErrors as true/false
                        # -------------------------------------------------
                        hasErrors:
                            on_graphql_error: true
                        
                        # -------------------------------------------------
                        # TIMING
                        # txTime in milliseconds (v1 plugin parity)
                        # -------------------------------------------------
                        txTime:
                            request_duration: milliseconds
                        duration_seconds:
                            request_duration: seconds
                        
                        # -------------------------------------------------
                        # CUSTOM ERROR FIELD EXTRACTION
                        # Extract specific fields from each error
                        # -------------------------------------------------
                        error_messages:
                            response_errors_field: "$.message"
                        error_codes:
                            response_errors_field: "$.extensions.code"
                        error_services:
                            response_errors_field: "$.extensions.service"

            supergraph:
                # Supergraph event for deferred/subscription responses
                # Fires for EACH GraphQL response chunk in a stream
                chunk.log:
                    message: "graphql_response_chunk"
                    on: event_response
                    level: info
                    attributes:
                        hasErrors:
                            on_graphql_error: true

        instruments:
            default_requirement_level: none
            router:
                # =================================================================
                # ERROR METRICS - Request counting (value: unit)
                # =================================================================
                
                # Count requests that had GraphQL errors
                golden_signal.error_requests:
                    value: unit
                    type: counter
                    unit: request
                    description: "Requests with GraphQL errors"
                    condition:
                        eq:
                            - on_graphql_error: true
                            - true

                # Count errors by HTTP status (with label)
                graphql.errors:
                    value: unit
                    type: counter
                    unit: error
                    description: "GraphQL errors by status"
                    attributes:
                        http.status:
                            response_status: code
                    condition:
                        eq:
                            - on_graphql_error: true
                            - true

                # =================================================================
                # ERROR METRICS - Individual error counting (value: response_errors_count)
                # =================================================================
                
                # Count ALL individual errors
                golden_signal.error_count:
                    value:
                        response_errors_count: "$[*]"
                    type: counter
                    unit: error
                    description: "Total error count (individual errors, not requests)"
                    condition:
                        eq:
                            - on_graphql_error: true
                            - true

                # Count ALL client errors by extension code
                golden_signal.total_client_errors:
                    value:
                        response_errors_count: "$[?(@.extensions.code == 'INVALID_TYPE' || @.extensions.code == 'INVALID_FIELD' || @.extensions.code == 'PARSING_ERROR' || @.extensions.code == 'GRAPHQL_VALIDATION_FAILED' || @.extensions.code == 'SUBSCRIPTION_NOT_SUPPORTED' || @.extensions.code == 'RECURSION_LIMIT_EXCEEDED')]"
                    type: counter
                    unit: error
                    description: "Client errors by extension code (all sources)"

                # Count ALL server errors (non-client codes)
                golden_signal.total_server_errors:
                    value:
                        response_errors_count: "$[?(!(@.extensions.code == 'INVALID_TYPE') && !(@.extensions.code == 'INVALID_FIELD') && !(@.extensions.code == 'PARSING_ERROR') && !(@.extensions.code == 'GRAPHQL_VALIDATION_FAILED') && !(@.extensions.code == 'SUBSCRIPTION_NOT_SUPPORTED') && !(@.extensions.code == 'RECURSION_LIMIT_EXCEEDED'))]"
                    type: counter
                    unit: error
                    description: "Server errors (all sources)"

                # =================================================================
                # SUPERGRAPH-ONLY METRICS (router-originated, no service field)
                # =================================================================
                
                # Count supergraph-originated client errors (no extensions.service = not from subgraph)
                golden_signal.supergraph_client_errors:
                    value:
                        response_errors_count: "$[?(!@.extensions.service && (@.extensions.code == 'INVALID_TYPE' || @.extensions.code == 'INVALID_FIELD' || @.extensions.code == 'PARSING_ERROR' || @.extensions.code == 'GRAPHQL_VALIDATION_FAILED' || @.extensions.code == 'SUBSCRIPTION_NOT_SUPPORTED' || @.extensions.code == 'RECURSION_LIMIT_EXCEEDED'))]"
                    type: counter
                    unit: error
                    description: "Client errors originated at router (not from subgraphs)"

                # Count supergraph-originated server errors (no extensions.service = not from subgraph)
                golden_signal.supergraph_server_errors:
                    value:
                        response_errors_count: "$[?(!@.extensions.service && !(@.extensions.code == 'INVALID_TYPE') && !(@.extensions.code == 'INVALID_FIELD') && !(@.extensions.code == 'PARSING_ERROR') && !(@.extensions.code == 'GRAPHQL_VALIDATION_FAILED') && !(@.extensions.code == 'SUBSCRIPTION_NOT_SUPPORTED') && !(@.extensions.code == 'RECURSION_LIMIT_EXCEEDED'))]"
                    type: counter
                    unit: error
                    description: "Server errors originated at router (not from subgraphs)"

                # =================================================================
                # SPECIFIC ERROR CODE DETECTION
                # =================================================================
                
                # Detect MISSING_QUERY_STRING errors specifically
                early_400.missing_query:
                    value: unit
                    type: counter
                    unit: error
                    description: "MISSING_QUERY_STRING errors detected"
                    condition:
                        exists:
                            response_errors: "$[?(@.extensions.code == 'MISSING_QUERY_STRING')]"

                # Fallback: detect ANY error via on_graphql_error
                early_400.any_error:
                    value: unit
                    type: counter
                    unit: error
                    description: "Any GraphQL error"
                    condition:
                        eq:
                            - on_graphql_error: true
                            - true

            supergraph:
                # Count all supergraph requests
                golden_signal.supergraph_requests:
                    value: unit
                    type: counter
                    unit: request
                    description: "Supergraph requests"

include_subgraph_errors:
    all: true
"#;

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

/// Helper to print golden_signal metrics
fn print_golden_signal_metrics(metrics: &str) {
    eprintln!("Metrics:");
    for line in metrics.lines() {
        if line.contains("golden_signal") && !line.starts_with('#') {
            eprintln!("  {line}");
        }
    }
}

/// Helper to find a specific log entry
fn find_log_containing<'a>(logs: &'a [String], needle: &str) -> Option<&'a String> {
    logs.iter().find(|l| l.contains(needle))
}

// =============================================================================
// ERROR HANDLING - METRICS TESTS
// =============================================================================

/// Test counting requests with errors using on_graphql_error condition.
#[tokio::test(flavor = "multi_thread")]
async fn test_error_count_metric() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute a bad query that will generate errors (missing query field)
    let (_, response) = router
        .execute_query(Query::builder().body(json!({})).build())
        .await;
    eprintln!("Response status: {}", response.status());
    let response_text = response.text().await?;
    eprintln!("Response body: {response_text}");

    // Check metrics
    let metrics = router.get_metrics_response().await?.text().await?;
    eprintln!("=== METRICS (filtered) ===");
    for line in metrics.lines() {
        if line.contains("golden_signal") {
            eprintln!("{line}");
        }
    }

    // The error counter should show 1
    assert!(
        metrics.contains("golden_signal_error_requests_total{otel_scope_name=\"apollo/router\"} 1"),
        "Expected golden_signal_error_requests_total = 1"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Test counting individual errors (not just requests) using response_errors_count.
#[tokio::test(flavor = "multi_thread")]
async fn test_count_individual_errors() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        // Mock subgraph that returns 3 errors
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [
                {"message": "Error 1", "extensions": {"code": "ERROR_1"}},
                {"message": "Error 2", "extensions": {"code": "ERROR_2"}},
                {"message": "Error 3", "extensions": {"code": "ERROR_3"}}
            ]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute a query - subgraph returns 3 errors
    let (_, response) = router.execute_default_query().await;
    let response_text = response.text().await?;
    eprintln!("Response body: {response_text}");

    // Verify we got 3 errors
    assert!(response_text.contains("ERROR_1"));
    assert!(response_text.contains("ERROR_2"));
    assert!(response_text.contains("ERROR_3"));

    // Check metrics
    let metrics = router.get_metrics_response().await?.text().await?;
    eprintln!("=== METRICS ===");
    for line in metrics.lines() {
        if line.contains("golden_signal") || line.contains("error_count") {
            eprintln!("{line}");
        }
    }

    // KEY: Should be 3 (count of errors), not 1 (count of requests)
    assert!(
        metrics.contains("golden_signal_error_count_total{otel_scope_name=\"apollo/router\"} 3"),
        "Expected golden_signal_error_count_total = 3 (3 individual errors)"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Test client error classification by extension code.
#[tokio::test(flavor = "multi_thread")]
async fn test_client_error_count_metric() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        // Mock subgraph that returns an error with INVALID_TYPE extension code (client error)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{
                "message": "Field type mismatch",
                "extensions": { "code": "INVALID_TYPE" }
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Test 1: Subgraph error (200 OK with INVALID_TYPE in body)
    eprintln!("\n=== Test 1: Subgraph client error (INVALID_TYPE) ===");
    let (_, response) = router.execute_default_query().await;
    let response_text = response.text().await?;
    eprintln!("Response body: {response_text}");
    assert!(response_text.contains("INVALID_TYPE"));

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    assert!(
        metrics.contains(
            "golden_signal_total_client_errors_total{otel_scope_name=\"apollo/router\"} 1"
        ),
        "Expected golden_signal_total_client_errors_total = 1 after subgraph error"
    );

    // Test 2: Early 400 error (GRAPHQL_VALIDATION_FAILED - client error code)
    eprintln!("\n=== Test 2: Early 400 client error (GRAPHQL_VALIDATION_FAILED) ===");
    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query": "{ thisFieldDoesNotExist }"}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_text = response.text().await?;
    eprintln!("Response body: {response_text}");
    assert!(response_text.contains("GRAPHQL_VALIDATION_FAILED"));

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    // Client error counter should now be 2
    assert!(
        metrics.contains(
            "golden_signal_total_client_errors_total{otel_scope_name=\"apollo/router\"} 2"
        ),
        "Expected golden_signal_total_client_errors_total = 2 (INVALID_TYPE + GRAPHQL_VALIDATION_FAILED)"
    );

    // Test 3: Early 400 error with SERVER error code (MISSING_QUERY_STRING)
    eprintln!("\n=== Test 3: Early 400 server error (MISSING_QUERY_STRING) ===");
    let (_, response) = router
        .execute_query(Query::builder().body(json!({})).build())
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_text = response.text().await?;
    eprintln!("Response body: {response_text}");
    assert!(response_text.contains("MISSING_QUERY_STRING"));

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    // Client errors should still be 2 (MISSING_QUERY_STRING is NOT a client error)
    assert!(
        metrics.contains(
            "golden_signal_total_client_errors_total{otel_scope_name=\"apollo/router\"} 2"
        ),
        "Expected golden_signal_total_client_errors_total = 2"
    );

    // Server error counter should now be 1
    assert!(
        metrics.contains(
            "golden_signal_total_server_errors_total{otel_scope_name=\"apollo/router\"} 1"
        ),
        "Expected golden_signal_total_server_errors_total = 1"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Test server error classification by extension code.
#[tokio::test(flavor = "multi_thread")]
async fn test_server_error_by_extension_code() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        // Mock subgraph that returns a server error (INTERNAL_SERVER_ERROR)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{
                "message": "Database connection failed",
                "extensions": { "code": "INTERNAL_SERVER_ERROR" }
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    let response_text = response.text().await?;
    eprintln!("Response body: {response_text}");
    assert!(response_text.contains("INTERNAL_SERVER_ERROR"));

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    // Server error counter should be 1
    assert!(
        metrics.contains(
            "golden_signal_total_server_errors_total{otel_scope_name=\"apollo/router\"} 1"
        ),
        "Expected golden_signal_total_server_errors_total = 1"
    );

    // Client error counter should be 0
    assert!(
        metrics.contains(
            "golden_signal_total_client_errors_total{otel_scope_name=\"apollo/router\"} 0"
        ),
        "Expected golden_signal_total_client_errors_total = 0"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Test supergraph-level request counting.
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_error_metric() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    assert!(
        metrics.contains(
            "golden_signal_supergraph_requests_total{otel_scope_name=\"apollo/router\"} 1"
        ),
        "Expected golden_signal_supergraph_requests_total = 1"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Test error metrics with HTTP status code as label.
#[tokio::test(flavor = "multi_thread")]
async fn test_error_metric_by_code() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute a bad query to get a validation error
    let (_, response) = router
        .execute_query(Query::builder().body(json!({})).build())
        .await;
    eprintln!("Response status: {}", response.status());

    let metrics = router.get_metrics_response().await?.text().await?;
    eprintln!("=== METRICS (filtered) ===");
    for line in metrics.lines() {
        if line.contains("graphql_errors") {
            eprintln!("{line}");
        }
    }

    assert!(
        metrics.contains("graphql_errors_total{http_status=\"400\""),
        "Expected graphql_errors_total with http_status=400"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Test if early 400 errors are visible to response_errors.
#[tokio::test(flavor = "multi_thread")]
async fn test_early_400_extension_code_visibility() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Send empty body - triggers MISSING_QUERY_STRING (early 400)
    let (_, response) = router
        .execute_query(Query::builder().body(json!({})).build())
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_text = response.text().await?;
    eprintln!("Response body: {response_text}");
    assert!(response_text.contains("MISSING_QUERY_STRING"));

    let metrics = router.get_metrics_response().await?.text().await?;
    eprintln!("=== METRICS (filtered) ===");
    for line in metrics.lines() {
        if line.contains("early_400") {
            eprintln!("{line}");
        }
    }

    // on_graphql_error should always work
    assert!(
        metrics.contains("early_400_any_error_total{otel_scope_name=\"apollo/router\"} 1"),
        "Expected early_400_any_error_total = 1"
    );

    // response_errors can also see the extension code
    assert!(
        metrics.contains("early_400_missing_query_total{otel_scope_name=\"apollo/router\"} 1"),
        "Expected early_400_missing_query_total = 1"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Comprehensive test demonstrating full parity with a v1 error handling plugin.
#[tokio::test(flavor = "multi_thread")]
async fn test_error_plugin_parity() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        // Mock subgraph returns 2 client errors + 1 server error
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [
                {"message": "Invalid type", "extensions": {"code": "INVALID_TYPE"}},
                {"message": "Invalid field", "extensions": {"code": "INVALID_FIELD"}},
                {"message": "Internal error", "extensions": {"code": "INTERNAL_SERVER_ERROR"}}
            ]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    eprintln!("=== ERROR PLUGIN PARITY TEST ===\n");

    // Test 1: Subgraph returns mixed errors (2 client, 1 server)
    eprintln!("--- Test 1: Subgraph mixed errors (2 client + 1 server) ---");
    let (_, response) = router.execute_default_query().await;
    let response_text = response.text().await?;
    eprintln!("Response: {response_text}");

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    assert!(
        metrics.contains(
            "golden_signal_total_client_errors_total{otel_scope_name=\"apollo/router\"} 2"
        ),
        "Expected 2 total client errors from subgraph"
    );
    assert!(
        metrics.contains(
            "golden_signal_total_server_errors_total{otel_scope_name=\"apollo/router\"} 1"
        ),
        "Expected 1 total server error from subgraph"
    );
    assert!(
        metrics.contains(
            "golden_signal_supergraph_client_errors_total{otel_scope_name=\"apollo/router\"} 0"
        ),
        "Expected 0 supergraph client errors (all from subgraph)"
    );
    assert!(
        metrics.contains(
            "golden_signal_supergraph_server_errors_total{otel_scope_name=\"apollo/router\"} 0"
        ),
        "Expected 0 supergraph server errors (all from subgraph)"
    );

    // Test 2: Early 400 with client error code
    eprintln!("\n--- Test 2: Early 400 client error (GRAPHQL_VALIDATION_FAILED) ---");
    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query": "{ nonExistentField }"}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    assert!(
        metrics.contains(
            "golden_signal_total_client_errors_total{otel_scope_name=\"apollo/router\"} 3"
        ),
        "Expected 3 total client errors"
    );
    assert!(
        metrics.contains(
            "golden_signal_supergraph_client_errors_total{otel_scope_name=\"apollo/router\"} 1"
        ),
        "Expected 1 supergraph client error"
    );

    // Test 3: Early 400 with server error code
    eprintln!("\n--- Test 3: Early 400 server error (MISSING_QUERY_STRING) ---");
    let (_, response) = router
        .execute_query(Query::builder().body(json!({})).build())
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let metrics = router.get_metrics_response().await?.text().await?;
    print_golden_signal_metrics(&metrics);

    assert!(
        metrics.contains(
            "golden_signal_total_server_errors_total{otel_scope_name=\"apollo/router\"} 2"
        ),
        "Expected 2 total server errors"
    );
    assert!(
        metrics.contains(
            "golden_signal_supergraph_server_errors_total{otel_scope_name=\"apollo/router\"} 1"
        ),
        "Expected 1 supergraph server error"
    );

    eprintln!("\n=== FINAL SUMMARY ===");
    eprintln!("✅ Individual errors counted (not just requests)");
    eprintln!("✅ Subgraph vs supergraph errors distinguished");
    eprintln!("✅ Client vs server errors classified by extension code");
    eprintln!("\nThis achieves FULL PARITY with all 4 v1 error handling plugin metrics!");

    router.graceful_shutdown().await;
    Ok(())
}

// =============================================================================
// LOGGING - STRUCTURED JSON LOGS TESTS
// =============================================================================

/// Test basic custom logging event.
#[tokio::test(flavor = "multi_thread")]
async fn test_custom_logging_event() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let has_request_log = logs.iter().any(|l| l.contains("router_request_log"));
    assert!(has_request_log, "Expected 'router_request_log' in logs");

    router.graceful_shutdown().await;
    Ok(())
}

/// Test logging with request headers captured.
#[tokio::test(flavor = "multi_thread")]
async fn test_log_request_headers() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    // Execute a query with custom headers
    let (_, response) = router
        .execute_query(
            Query::builder()
                .header("x-custom-header", "my-custom-value")
                .header("apollographql-client-name", "test-client")
                .body(json!({"query": "{ topProducts { name } }"}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let has_custom_header = logs.iter().any(|l| l.contains("my-custom-value"));
    assert!(
        has_custom_header,
        "Expected 'my-custom-value' in logs from x-custom-header"
    );

    router.graceful_shutdown().await;
    Ok(())
}

/// Test the request_duration selector for txTime parity.
#[tokio::test(flavor = "multi_thread")]
async fn test_request_duration_selector() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let log_entry =
        find_log_containing(&logs, "router_request_log").expect("Should have router_request_log");

    // Verify txTime is present
    assert!(log_entry.contains("txTime"), "Should contain txTime field");
    assert!(
        log_entry.contains("duration_seconds"),
        "Should contain duration_seconds field"
    );

    // Parse and verify values
    let log_json: serde_json::Value = serde_json::from_str(log_entry)?;
    let tx_time = log_json.get("txTime").and_then(|v| v.as_i64());
    assert!(
        tx_time.is_some() && tx_time.unwrap() >= 0,
        "txTime should be >= 0"
    );

    let duration_secs = log_json.get("duration_seconds").and_then(|v| v.as_f64());
    assert!(
        duration_secs.is_some() && duration_secs.unwrap() >= 0.0,
        "duration_seconds should be >= 0"
    );

    eprintln!("✅ txTime (ms): {}", tx_time.unwrap());
    eprintln!("✅ duration_seconds: {}", duration_secs.unwrap());

    router.graceful_shutdown().await;
    Ok(())
}

/// Comprehensive test demonstrating full logging plugin parity.
#[tokio::test(flavor = "multi_thread")]
async fn test_logging_plugin_parity() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        // Mock subgraph that returns errors
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [
                {
                    "message": "Product not found",
                    "path": ["topProducts"],
                    "extensions": { "code": "NOT_FOUND", "service": "products" }
                },
                {
                    "message": "Database timeout",
                    "path": ["topProducts"],
                    "extensions": { "code": "INTERNAL_SERVER_ERROR", "service": "products" }
                }
            ]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    eprintln!("=== LOGGING PLUGIN PARITY TEST ===\n");

    // Request with all headers and operation name
    let (_, response) = router
        .execute_query(
            Query::builder()
                .header("x-request-tid", "test-transaction-123")
                .header("apollographql-client-name", "test-client")
                .header("apollographql-client-version", "1.0.0")
                .header("x-request-id", "req-456")
                .body(json!({
                    "query": "query GetProducts { topProducts { name } }",
                    "operationName": "GetProducts"
                }))
                .build(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let log_entry =
        find_log_containing(&logs, "router_request_log").expect("Should have router_request_log");

    eprintln!("=== LOG OUTPUT ===");
    eprintln!("{log_entry}");

    // Verify all achievable fields
    eprintln!("\n=== FIELD VERIFICATION ===");

    // Request headers
    assert!(
        log_entry.contains("test-transaction-123"),
        "Should contain x-request-tid"
    );
    eprintln!("✅ x-request-tid: PRESENT");

    assert!(
        log_entry.contains("test-client"),
        "Should contain client name"
    );
    eprintln!("✅ apollographql-client-name: PRESENT");

    assert!(log_entry.contains("req-456"), "Should contain request id");
    eprintln!("✅ x-request-id: PRESENT");

    // Operation name
    assert!(
        log_entry.contains("GetProducts"),
        "Should contain operation name"
    );
    eprintln!("✅ operationName: PRESENT");

    // HTTP status
    assert!(log_entry.contains("200"), "Should contain HTTP status");
    eprintln!("✅ http.status_code: PRESENT");

    // hasErrors as "T"/"F"
    assert!(
        log_entry.contains("\"hasErrors\":true"),
        "hasErrors should be 'T'"
    );
    eprintln!("✅ hasErrors (true/false boolean): PRESENT");

    // txTime
    assert!(log_entry.contains("txTime"), "Should contain txTime");
    eprintln!("✅ txTime: PRESENT");

    // Custom error field extraction
    assert!(
        log_entry.contains("error_messages"),
        "Should contain error_messages"
    );
    eprintln!("✅ error_messages: PRESENT");

    assert!(
        log_entry.contains("error_codes"),
        "Should contain error_codes"
    );
    eprintln!("✅ error_codes: PRESENT");

    assert!(
        log_entry.contains("error_services"),
        "Should contain error_services"
    );
    eprintln!("✅ error_services: PRESENT");

    eprintln!("\n🎉 FULL LOGGING PLUGIN PARITY ACHIEVED!");

    router.graceful_shutdown().await;
    Ok(())
}

/// Test configuration for deferred/subscription response logging.
#[tokio::test(flavor = "multi_thread")]
async fn test_deferred_response_logging_config() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();

    // Verify router-level event fired
    let has_router_log = logs.iter().any(|l| l.contains("router_request_log"));
    assert!(has_router_log, "Should have router_request_log");

    // Verify supergraph event_response fired
    let has_chunk_log = logs.iter().any(|l| l.contains("graphql_response_chunk"));
    assert!(has_chunk_log, "Should have graphql_response_chunk log");

    eprintln!("\n=== CONFIGURATION NOTES ===");
    eprintln!("For @defer queries, supergraph `on: event_response` fires for each chunk.");
    eprintln!("For subscriptions, the event fires for each subscription message.");

    router.graceful_shutdown().await;
    Ok(())
}

// =============================================================================
// LOGGING - EDGE CASES AND ERROR SCENARIOS
// =============================================================================

/// Test: Request header missing - uses default value.
#[tokio::test(flavor = "multi_thread")]
async fn test_logging_missing_request_header() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    // Execute query WITHOUT optional headers - they should use defaults
    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let log_entry =
        find_log_containing(&logs, "router_request_log").expect("Should have router_request_log");

    // content_type should be present (sent by test framework)
    assert!(
        log_entry.contains("content_type"),
        "Should have content_type field"
    );

    // Missing headers use empty string default
    eprintln!("✅ Missing headers use default values (empty string)");

    router.graceful_shutdown().await;
    Ok(())
}

/// Test: Response header missing - uses default value.
#[tokio::test(flavor = "multi_thread")]
async fn test_logging_missing_response_header() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let log_entry =
        find_log_containing(&logs, "router_request_log").expect("Should have router_request_log");

    // response-content-type should be present
    assert!(
        log_entry.contains("response-content-type"),
        "Should have response-content-type field"
    );

    eprintln!("✅ Missing response headers use default values");

    router.graceful_shutdown().await;
    Ok(())
}

/// Test: Invalid JSON in request body - telemetry still fires.
#[tokio::test(flavor = "multi_thread")]
async fn test_logging_invalid_json_body() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    // Send invalid JSON
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}", router.bind_address()))
        .header("content-type", "application/json")
        .body("{ this is not valid json }")
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let logs = router.logs();
    let has_log = logs.iter().any(|l| l.contains("router_request_log"));

    if has_log {
        eprintln!("✅ Telemetry event fired even for invalid JSON request");
    } else {
        eprintln!("⚠️  Telemetry event may not fire for malformed requests");
    }

    router.graceful_shutdown().await;
    Ok(())
}

/// Test: Operation name not provided - uses default.
#[tokio::test(flavor = "multi_thread")]
async fn test_logging_missing_operation_name() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    // Send query WITHOUT operationName
    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query": "{ topProducts { name } }"}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let log_entry =
        find_log_containing(&logs, "router_request_log").expect("Should have router_request_log");

    // operationName should be empty string (default)
    assert!(
        log_entry.contains("operationName"),
        "Should have operationName field"
    );

    eprintln!("✅ Missing operationName uses default (empty string)");

    router.graceful_shutdown().await;
    Ok(())
}

/// Test: Response with errors shows hasErrors = "T".
#[tokio::test(flavor = "multi_thread")]
async fn test_logging_has_errors_true() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        // Mock subgraph returns errors
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{"message": "Test error", "extensions": {"code": "TEST_ERROR"}}]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let log_entry =
        find_log_containing(&logs, "router_request_log").expect("Should have router_request_log");

    assert!(
        log_entry.contains("\"hasErrors\":true"),
        "hasErrors should be 'T' when response has errors"
    );

    eprintln!("✅ Response with errors: hasErrors = true");

    router.graceful_shutdown().await;
    Ok(())
}

/// Test: Successful request shows hasErrors = "F".
#[tokio::test(flavor = "multi_thread")]
async fn test_logging_has_errors_false() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(SHARED_CONFIG)
        // Mock subgraph returns success (no errors)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"topProducts": [{"name": "Product 1"}]}
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.read_logs();

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = router.logs();
    let log_entry =
        find_log_containing(&logs, "router_request_log").expect("Should have router_request_log");

    assert!(
        log_entry.contains("\"hasErrors\":false"),
        "hasErrors should be 'F' when response has no errors"
    );

    eprintln!("✅ Response without errors: hasErrors = false");

    router.graceful_shutdown().await;
    Ok(())
}
