use std::time::Duration;

use prost::Message;
use tower::BoxError;

use super::find_metric_in_request;
use super::mock_otlp_server;
use crate::integration::IntegrationTest;
use crate::integration::common::Telemetry;

/// Validates that a metric is an updown counter with cumulative temporality.
/// Returns `true` if the metric was found and validated, `false` otherwise.
fn validate_updown_counter_metric(
    metric_name: &str,
    metrics: &opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest,
    iteration: &str,
) -> bool {
    let Some(metric) = find_metric_in_request(metric_name, metrics) else {
        return false;
    };

    // Verify it's a Sum (updown counters are exported as Sum)
    let Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(sum)) = &metric.data else {
        panic!(
            "{} should be a Sum metric on iteration {}",
            metric_name, iteration
        );
    };

    // Verify temporality is Cumulative (value = 2)
    assert_eq!(
        sum.aggregation_temporality, 2,
        "{} should have Cumulative temporality (2), got {} on iteration {}",
        metric_name, sum.aggregation_temporality, iteration
    );

    // Verify it's not monotonic (updown counters can go up and down)
    assert!(
        !sum.is_monotonic,
        "{} should not be monotonic on iteration {}",
        metric_name, iteration
    );

    true
}

/// Checks if a metric exists and validates it across all OTLP metric requests.
/// Returns `true` if the metric was found in any request, `false` otherwise.
fn find_and_validate_metric<T>(metric_name: &str, requests: &[T], iteration: &str) -> bool
where
    T: AsRef<[u8]>,
{
    requests.iter().any(|request| {
        let Ok(metrics) =
            opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest::decode(
                bytes::Bytes::copy_from_slice(request.as_ref()),
            )
        else {
            return false;
        };
        validate_updown_counter_metric(metric_name, &metrics, iteration)
    })
}

/// Executes a query and validates that updown counter metrics are present and correct.
///
/// The poll loop deliberately checks for *metric content*, not just *batch
/// arrival*. The previous implementation broke out of the loop as soon as any
/// `/metrics` request landed and then asserted on its contents — but a single
/// batch is not guaranteed to contain every UpDownCounter we expect. Under
/// `temporality: delta`, an UpDownCounter only emits a data point when its
/// value changes since the last export, so the very first batch after
/// `execute_default_query()` may carry `apollo.router.pipelines` but not
/// `apollo.router.open_connections` if the test client's connection opens and
/// closes inside one export window. We must keep accumulating batches until
/// either the metric appears or the deadline passes.
async fn execute_and_validate_metrics(
    router: &mut IntegrationTest,
    mock_server: &wiremock::MockServer,
    iteration: &str,
) -> Result<(), BoxError> {
    router.execute_default_query().await;

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let expected_metrics = ["apollo.router.pipelines", "apollo.router.open_connections"];

    let mut last_seen_metrics: Vec<String> = Vec::new();
    let mut last_batch_count: usize;
    loop {
        let requests = mock_server
            .received_requests()
            .await
            .expect("Could not get otlp requests");

        let metrics_requests: Vec<_> = requests
            .into_iter()
            .filter(|r| r.url.path().ends_with("/metrics"))
            .collect();
        last_batch_count = metrics_requests.len();

        if !metrics_requests.is_empty() {
            let request_bodies: Vec<_> = metrics_requests.iter().map(|r| &r.body).collect();

            last_seen_metrics = metrics_requests
                .iter()
                .flat_map(|r| {
                    opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest::decode(
                        bytes::Bytes::copy_from_slice(&r.body),
                    )
                    .ok()
                })
                .flat_map(|m| m.resource_metrics)
                .flat_map(|rm| rm.scope_metrics)
                .flat_map(|sm| sm.metrics)
                .map(|m| m.name)
                .collect();

            let all_found = expected_metrics
                .iter()
                .all(|name| find_and_validate_metric(name, &request_bodies, iteration));
            if all_found {
                return Ok(());
            }
        }

        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    panic!(
        "expected updown counter metrics {expected:?} not all present in OTLP exports \
         on iteration {iteration} (saw {batches} batch(es), distinct metric names: {seen:?})",
        expected = expected_metrics,
        iteration = iteration,
        batches = last_batch_count,
        seen = last_seen_metrics,
    );
}

/// Helper function to test that updown counters always use cumulative temporality
/// regardless of the configured temporality for other metrics.
async fn test_updown_counter_with_temporality(config: &str) -> Result<(), BoxError> {
    let mock_server = mock_otlp_server(1..).await;
    let config = config.replace("<otel-collector-endpoint>", &mock_server.uri());

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Validate metrics after initial start
    execute_and_validate_metrics(&mut router, &mock_server, "initial start").await?;

    // Reload configuration and verify no errors
    router.touch_config().await;
    router.assert_reloaded().await;
    router.assert_log_not_contained(
        "OpenTelemetry metric error occurred: Metrics error: metrics provider already shut down",
    );

    // Validate metrics after reload
    execute_and_validate_metrics(&mut router, &mock_server, "after reload").await?;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_updown_counter_temporality_with_cumulative() -> Result<(), BoxError> {
    let config = include_str!("../fixtures/otlp.router.yaml");
    test_updown_counter_with_temporality(config).await
}

#[tokio::test(flavor = "multi_thread")]
async fn test_updown_counter_temporality_with_delta() -> Result<(), BoxError> {
    let config = include_str!("../fixtures/otlp-delta.router.yaml");
    test_updown_counter_with_temporality(config).await
}
