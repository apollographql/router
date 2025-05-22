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
