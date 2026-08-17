use tower::BoxError;
use uuid::Uuid;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;

#[tokio::test(flavor = "multi_thread")]
async fn test_json() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/json.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.wait_for_log_message("trace_id").await;
    router.execute_default_query().await;
    router.wait_for_log_message("span_id").await;
    router.execute_default_query().await;
    router.wait_for_log_message(r#""static_one":"test""#).await;
    #[cfg(unix)]
    {
        router.execute_default_query().await;
        router
            .wait_for_log_message(
                r#""schema.id":"dd8960ccefda82ca58e8ac0bc266459fd49ee8215fd6b3cc72e7bc3d7f3464b9""#,
            )
            .await;
    }

    router.execute_default_query().await;
    router
        .wait_for_log_message(r#""on_supergraph_response_event":"on_supergraph_event""#)
        .await;
    router.execute_default_query().await;
    router
        .wait_for_log_message(r#""response_status":200"#)
        .await;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_json_promote_span_attributes() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/json.span_attributes.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.wait_for_log_message("trace_id").await;
    router.execute_query(Query::default()).await;
    router.wait_for_log_message("span_id").await;
    router.execute_default_query().await;
    router.wait_for_log_message(r#""static_one":"test""#).await;
    router.execute_default_query().await;
    router
        .wait_for_log_message(r#""response_status":200"#)
        .await;
    router.execute_default_query().await;
    router.wait_for_log_message(r#""too_big":true"#).await;
    router.execute_default_query().await;
    router.wait_for_log_message(r#""too_big":"nope""#).await;
    router.execute_default_query().await;
    router
        .wait_for_log_message(r#""graphql.document":"query ExampleQuery {topProducts{name}}""#)
        .await;
    router.execute_default_query().await;
    router.assert_log_not_contains(r#""should_not_log""#).await;
    router.assert_log_not_contains(r#""another_one""#).await;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_json_uuid_format() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/json.uuid.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.wait_for_log_message("trace_id").await;
    let (trace_id, _) = router.execute_default_query().await;
    router
        .wait_for_log_message(&format!("{}", Uuid::from_bytes(trace_id.to_bytes())))
        .await;
    router.execute_default_query().await;
    router.wait_for_log_message("span_id").await;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_text_uuid_format() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/text.uuid.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.wait_for_log_message("trace_id").await;
    let (trace_id, _) = router.execute_default_query().await;
    router
        .wait_for_log_message(&format!("{}", Uuid::from_bytes(trace_id.to_bytes())))
        .await;
    router.execute_default_query().await;
    router.wait_for_log_message("span_id").await;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_json_sampler_off() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/json.sampler_off.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.wait_for_log_message("trace_id").await;
    router.execute_default_query().await;
    router.wait_for_log_message("span_id").await;
    router.execute_default_query().await;
    router.wait_for_log_message(r#""static_one":"test""#).await;
    router.execute_default_query().await;
    router
        .wait_for_log_message(r#""on_supergraph_response_event":"on_supergraph_event""#)
        .await;
    router.execute_default_query().await;
    router
        .wait_for_log_message(r#""response_status":200"#)
        .await;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_text() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/text.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_query(Query::default()).await;
    router.execute_query(Query::default()).await;
    router.wait_for_log_message("trace_id").await;
    router.execute_query(Query::default()).await;
    router.wait_for_log_message("span_id").await;
    router
        .wait_for_log_message(r#"on_supergraph_response_event=on_supergraph_event"#)
        .await;
    router.execute_query(Query::default()).await;
    router.execute_query(Query::default()).await;
    router.wait_for_log_message("response_status=200").await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_text_sampler_off() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/text.sampler_off.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.execute_default_query().await;
    router.wait_for_log_message("trace_id").await;
    router.execute_default_query().await;
    router.wait_for_log_message("span_id").await;
    router.execute_default_query().await;
    router.wait_for_log_message("response_status=200").await;
    router.graceful_shutdown().await;
    Ok(())
}

/// The `Authorization error` event for a refused operation belongs to the
/// `query_planning` span, where the query planner decides the refusal. The unit tests
/// around authorization cannot pin the span: it takes the telemetry plugin, which only
/// joins the pipeline when OpenTelemetry is initialised for the process, so a spawned
/// router is the smallest thing that has the full span hierarchy.
#[tokio::test(flavor = "multi_thread")]
async fn test_authorization_error_event_in_query_planning_span() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/authorization_error_span.router.yaml"
        ))
        .supergraph("tests/fixtures/supergraph-auth.graphql")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // `Query.me` requires the `profile` scope, so an unauthenticated request loses its
    // only root field and authorization refuses the operation.
    router
        .execute_query(
            Query::builder()
                .body(serde_json::json!({ "query": "{ me { name } }" }))
                .build(),
        )
        .await;
    router.wait_for_log_message("Authorization error").await;

    let events: Vec<serde_json::Value> = router
        .logs()
        .iter()
        .filter(|line| line.contains("Authorization error"))
        .map(|line| serde_json::from_str(line).expect("log line is JSON"))
        .collect();

    // Exactly once: a refusal is decided at a single place, so a second event for the
    // same request means two code paths both believe they own the log.
    assert_eq!(events.len(), 1, "events: {events:?}");

    let event = &events[0];
    // The event records the paths with `?`, so they arrive debug-formatted as one string.
    assert_eq!(
        event.pointer("/unauthorized_query_paths"),
        Some(&serde_json::json!(r#"["/me"]"#))
    );
    let span_names: Vec<&str> = event
        .pointer("/spans")
        .and_then(|spans| spans.as_array())
        .expect("json logs carry a span list")
        .iter()
        .filter_map(|span| span.get("name").and_then(|name| name.as_str()))
        .collect();
    assert!(
        span_names.contains(&"query_planning"),
        "expected the event inside the query_planning span, got spans: {span_names:?}"
    );

    router.graceful_shutdown().await;
    Ok(())
}
