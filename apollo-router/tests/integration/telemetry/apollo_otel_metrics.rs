use std::time::Duration;

use opentelemetry_proto::tonic::metrics::v1::metric;
use opentelemetry_proto::tonic::metrics::v1::number_data_point;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_error() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(
            r#"
            include_subgraph_errors:
                all: true
            telemetry:
                apollo:
                    experimental_otlp_metrics_protocol: http
                    batch_processor:
                        scheduled_delay: 1s # lowering this seems to make the test flaky
                    errors:
                        preview_extended_error_metrics: enabled
            "#,
        )
        .responder(ResponseTemplate::new(500).append_header("Content-Type", "application/json"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_query(Query::default()).await;

    let response = response.text().await.unwrap();
    assert!(response.contains("SUBREQUEST_HTTP_ERROR"));

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;
    assert!(!metrics.is_empty());
    let mut error_count = 0;

    // Ideally this would make use of something like the assert_counter! macro to assert the error count
    metrics.iter().for_each(|m| {
        m.resource_metrics.iter().for_each(|rm| {
            rm.scope_metrics.iter().for_each(|sm| {
                sm.metrics.iter().for_each(|m| {
                    if m.name == "apollo.router.operations.error" {
                        if let Some(metric::Data::Sum(sum)) = &m.data {
                            sum.data_points.iter().for_each(|dp| {
                                if let Some(number_data_point::Value::AsInt(value)) = dp.value {
                                    error_count += value;
                                }
                            });
                        }
                    }
                });
            });
        });
    });
    assert_eq!(error_count, 2); // 1 error from each subgraph
    router.graceful_shutdown().await;
}
