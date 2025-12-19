use std::path::PathBuf;
use std::time::Duration;

use http::StatusCode;
use insta::assert_yaml_snapshot;
use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;

#[tokio::test(flavor = "multi_thread")]
async fn test_router_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            traffic_shaping:
                router:
                    timeout: 1ns
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 504);
    let response = response.text().await?;
    assert!(response.contains("GATEWAY_TIMEOUT"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"http_server_request_duration_seconds_count{error_type="Gateway Timeout",http_request_method="POST",http_response_status_code="504""#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            include_subgraph_errors:
                all: true
            traffic_shaping:
                all:
                    timeout: 1ns
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(response.contains("GATEWAY_TIMEOUT"));
    assert_yaml_snapshot!(response);

    // We need to add support for http.client metrics ROUTER-991
    //router.assert_metrics_contains(r#"apollo_router_graphql_error_total{code="REQUEST_TIMEOUT",otel_scope_name="apollo/router"} 1"#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connector_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            traffic_shaping:
                connector:
                    sources:
                        connectors.jsonPlaceholder:
                            timeout: 1ns
            include_subgraph_errors:
                all: true
            "#,
        )
        .supergraph(PathBuf::from_iter([
            "..",
            "apollo-router",
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query":"query ExampleQuery {posts{id}}","variables":{}}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(response.contains("GATEWAY_TIMEOUT"));
    assert_yaml_snapshot!(response);

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_timeout_operation_name_in_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            traffic_shaping:
                router:
                    # NB: Normally in tests we would set the timeout to 1ns. But here,
                    # we are testing a feature that requires GraphQL parsing. If the timeout
                    # is set to almost 0, then we might time out well before we get to the parser.
                    # This value could still be racey, but hopefully we can get away with it.
                    timeout: 100ms
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(250)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({
                    "query": "query UniqueName { topProducts { name } }"
                }))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 504);
    let response = response.text().await?;
    assert!(response.contains("GATEWAY_TIMEOUT"));

    router
        .wait_for_log_message(r#""otel.name":"query UniqueName""#)
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_custom_metric() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
                instrumentation:
                    instruments:
                        router:
                            http.server.request.duration:
                                attributes:
                                    # Standard attributes
                                    http.response.status_code: true
                                    graphql.error:
                                        on_graphql_error: true
            traffic_shaping:
                router:
                    timeout: 1ns
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(Query::default().with_bad_query())
        .await;
    let response = response.text().await?;
    assert!(response.contains("MISSING_QUERY_STRING"));
    router.assert_metrics_contains(r#"http_server_request_duration_seconds_count{error_type="Bad Request",graphql_error="true",http_request_method="POST",http_response_status_code="400""#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_rate_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            traffic_shaping:
                router:
                    global_rate_limit:
                        capacity: 1
                        interval: 10min
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(!response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 429);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"http_server_request_duration_seconds_count{error_type="Too Many Requests",http_request_method="POST",http_response_status_code="429""#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_rate_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            include_subgraph_errors:
                all: true
            traffic_shaping:
                all:
                    global_rate_limit:
                        capacity: 1
                        interval: 10min
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(!response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"apollo_router_graphql_error_total{code="REQUEST_RATE_LIMITED",otel_scope_name="apollo/router"} 1"#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connector_rate_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            include_subgraph_errors:
                all: true
            traffic_shaping:
                connector:
                    sources:
                        connectors.jsonPlaceholder:
                            global_rate_limit:
                                capacity: 1
                                interval: 10min
            connectors:
                sources:
                    connectors.jsonPlaceholder:
                        $config:
                            my.config.value: true
            "#,
        )
        .supergraph(PathBuf::from_iter([
            "..",
            "apollo-router",
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
            "body:": "This is a really great post",
            "userId": 1
        }])))
        .http_method("GET")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query":"query ExampleQuery {posts{id}}","variables":{}}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(!response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query":"query ExampleQuery {posts{id}}","variables":{}}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"apollo_router_graphql_error_total{code="REQUEST_RATE_LIMITED",otel_scope_name="apollo/router"} 1"#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_custom_plugins_will_not_see_ratelimited_requests() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            traffic_shaping:
                router:
                    global_rate_limit:
                        capacity: 1
                        interval: 10min
            rhai:
                scripts: "tests/integration/fixtures"
                main: "stage_logger.rhai"
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let rhai_request_observed = router
        .logs()
        .iter()
        .filter(|line| line.contains("rhai router request"))
        .count();

    let rhai_response_observed = router
        .logs()
        .iter()
        .filter(|line| line.contains("rhai router response"))
        .count();

    // we sent two requests, but one was rejected immediately due to rate-limiting - so only one reaches
    // the rhai plugin
    assert_eq!(rhai_request_observed, 1);
    assert_eq!(rhai_response_observed, 1);

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_custom_telemetry_will_see_ratelimited_requests() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            traffic_shaping:
                router:
                    global_rate_limit:
                        capacity: 1
                        interval: 10min
            telemetry:
                instrumentation:
                    instruments:
                        default_requirement_level: none
                        router:
                            golden_signal.total_server_errors:
                                value: unit
                                type: counter
                                unit: "{error}"
                                description: "Count of router responses with errors that do not include `NOT_FOUND`, `INVALID_TYPE`"
                                condition:
                                    exists:
                                        response_errors: "$[?(!(@.extensions.code == 'NOT_FOUND' || @.extensions.code == 'INVALID_TYPE'))]"
                            # NB: this is not the number of client errors, it is the number of router responses which include client errors
                            golden_signal.total_client_errors:
                                value: unit
                                type: counter
                                unit: "{error}"
                                description: "Count of router responses with errors that include `NOT_FOUND`, `INVALID_TYPE`"
                                condition:
                                    exists:
                                        response_errors: "$[?(@.extensions.code == 'NOT_FOUND' || @.extensions.code == 'INVALID_TYPE')]"
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
        "#,
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::OK);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // there should be zero client errors and one server error
    router
        .assert_metrics_does_not_contain(
            "golden_signal_total_client_errors_total{otel_scope_name=<any>}",
        )
        .await;

    router
        .assert_metrics_contains(
            "golden_signal_total_server_errors_total{otel_scope_name=<any>} 1",
            None,
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_custom_telemetry_will_see_timed_out_requests() -> Result<(), BoxError> {
    // NB: steals timeout inducer (the responder) from `test_router_timeout_operation_name_in_tracing`
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            traffic_shaping:
                router:
                    timeout: 100ms
            telemetry:
                instrumentation:
                    instruments:
                        default_requirement_level: none
                        router:
                            golden_signal.total_server_errors:
                                value: unit
                                type: counter
                                unit: "{error}"
                                description: "Count of router responses with errors that do not include `NOT_FOUND`, `INVALID_TYPE`"
                                condition:
                                    exists:
                                        response_errors: "$[?(!(@.extensions.code == 'NOT_FOUND' || @.extensions.code == 'INVALID_TYPE'))]"
                            # NB: this is not the number of client errors, it is the number of router responses which include client errors
                            golden_signal.total_client_errors:
                                value: unit
                                type: counter
                                unit: "{error}"
                                description: "Count of router responses with errors that include `NOT_FOUND`, `INVALID_TYPE`"
                                condition:
                                    exists:
                                        response_errors: "$[?(@.extensions.code == 'NOT_FOUND' || @.extensions.code == 'INVALID_TYPE')]"

                            golden_signal.total_server_errors_indiv:
                                value:
                                    operator:
                                        array_length:
                                            response_errors: "$[?(!(@.extensions.code == 'NOT_FOUND' || @.extensions.code == 'INVALID_TYPE'))]"
                                type: counter
                                unit: "{error}"
                                description: "Count of router responses with errors that do not include `NOT_FOUND`, `INVALID_TYPE`"

                            golden_signal.total_client_errors_indiv:
                                value:
                                    operator:
                                        array_length:
                                            response_errors: "$[?(@.extensions.code == 'NOT_FOUND' || @.extensions.code == 'INVALID_TYPE')]"
                                type: counter
                                unit: "{error}"
                                description: "Count of router responses with errors that include `NOT_FOUND`, `INVALID_TYPE`"
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(250)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);

    // there should be zero client errors and two server errors
    router
        .assert_metrics_does_not_contain(
            "golden_signal_total_client_errors_total{otel_scope_name=<any>}",
        )
        .await;

    router
        .assert_metrics_contains(
            "golden_signal_total_server_errors_total{otel_scope_name=<any>} 2",
            None,
        )
        .await;

    router
        .assert_metrics_contains(
            "golden_signal_total_server_errors_indiv_total{otel_scope_name=<any>} 2",
            None,
        )
        .await;
    router
        .assert_metrics_does_not_contain(
            "golden_signal_total_client_errors_indiv_total{otel_scope_name=<any>}",
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_custom_plugins_will_not_see_timed_out_requests() -> Result<(), BoxError> {
    // NB: steals timeout inducer (the responder) from `test_router_timeout_operation_name_in_tracing`
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            traffic_shaping:
                router:
                    timeout: 100ms
            rhai:
                scripts: "tests/integration/fixtures"
                main: "stage_logger.rhai"
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(250)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);

    let rhai_request_observed = router
        .logs()
        .iter()
        .filter(|line| line.contains("rhai router request"))
        .count();

    let rhai_response_observed = router
        .logs()
        .iter()
        .filter(|line| line.contains("rhai router response"))
        .count();

    router.print_logs();

    // we sent two requests, which were both observed on the request side.
    // but the timeout short-circuits custom plugins, so no responses were observed.
    assert_eq!(rhai_request_observed, 2);
    assert_eq!(rhai_response_observed, 0);

    router.graceful_shutdown().await;
    Ok(())
}
