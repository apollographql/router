use serde_json::json;
use tower::BoxError;
use uuid::Uuid;

use crate::integration::common::graph_os_enabled;
use crate::integration::common::IntegrationTest;
use crate::integration::common::Telemetry;

#[tokio::test(flavor = "multi_thread")]
async fn test_json() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/json.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""static_one":"test""#).await;
    #[cfg(unix)]
    {
        router.execute_query(&query).await;
        router
            .assert_log_contains(
                r#""schema.id":"dd8960ccefda82ca58e8ac0bc266459fd49ee8215fd6b3cc72e7bc3d7f3464b9""#,
            )
            .await;
    }

    router.execute_query(&query).await;
    router
        .assert_log_contains(r#""on_supergraph_response_event":"on_supergraph_event""#)
        .await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""response_status":200"#).await;
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
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/json.span_attributes.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""static_one":"test""#).await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""response_status":200"#).await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""too_big":true"#).await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""too_big":"nope""#).await;
    router.execute_query(&query).await;
    router
        .assert_log_contains(r#""graphql.document":"query ExampleQuery {topProducts{name}}""#)
        .await;
    router.execute_query(&query).await;
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
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/json.uuid.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    let (trace_id, _) = router.execute_query(&query).await;
    router
        .assert_log_contains(&format!("{}", Uuid::from_bytes(trace_id.to_bytes())))
        .await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
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
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/text.uuid.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    let (trace_id, _) = router.execute_query(&query).await;
    router
        .assert_log_contains(&format!("{}", Uuid::from_bytes(trace_id.to_bytes())))
        .await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
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
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/json.sampler_off.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""static_one":"test""#).await;
    router.execute_query(&query).await;
    router
        .assert_log_contains(r#""on_supergraph_response_event":"on_supergraph_event""#)
        .await;
    router.execute_query(&query).await;
    router.assert_log_contains(r#""response_status":200"#).await;
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
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/text.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
    router
        .assert_log_contains(r#"on_supergraph_response_event=on_supergraph_event"#)
        .await;
    router.execute_query(&query).await;
    router.execute_query(&query).await;
    router.assert_log_contains("response_status=200").await;
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
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/text.sampler_off.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("response_status=200").await;
    router.graceful_shutdown().await;
    Ok(())
}
