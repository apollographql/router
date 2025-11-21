use serde_json::json;
use tower::BoxError;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;

#[tokio::test(flavor = "multi_thread")]
async fn test_trace_id_via_header() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    async fn make_call(router: &mut IntegrationTest, trace_id: &str) {
        let _ = router.execute_query(Query::builder().body(json!({"query":"query {topProducts{name, name, name, name, name, name, name, name, name, name}}","variables":{}})).header("id_from_header".to_string(), trace_id.to_string()).build()).await;
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::None)
        .config(include_str!("fixtures/trace_id_via_header.router.yaml"))
        .build()
        .await;

    let trace_id = "00000000000000000000000000000001";
    router.start().await;
    router.assert_started().await;
    make_call(&mut router, trace_id).await;
    router
        .wait_for_log_message(&format!("trace_id: {trace_id}"))
        .await;

    make_call(&mut router, trace_id).await;
    router
        .wait_for_log_message(&format!("\"id_from_header\": \"{trace_id}\""))
        .await;

    router.graceful_shutdown().await;
    Ok(())
}
