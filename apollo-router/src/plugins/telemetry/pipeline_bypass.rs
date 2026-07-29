//! Telemetry for requests answered without entering the router pipeline.
//!
//! The `http.server.*` instruments live inside the router pipeline, so a request rejected or shed
//! before it gets there is invisible to them. Recording the same duration histogram here keeps
//! those requests visible alongside the ones that made it through.

use std::time::Duration;
use std::time::Instant;

use crate::plugins::telemetry::span_factory;

/// Records `http.server.request.duration` for a request that never entered the router pipeline.
///
/// The metric name, unit, and description must stay identical to the histogram
/// `RouterInstruments` builds from `HTTP_SERVER_REQUEST_DURATION_METRIC`, so that APM tools
/// aggregate both together. The macro takes literals, so the compiler cannot tie them together —
/// `pipeline_bypass_metric_matches_router_instrument` asserts they agree instead.
pub(crate) fn record_bypassed_request(status_code: u16, duration: Duration) {
    f64_histogram_with_unit!(
        "http.server.request.duration",
        "Duration of HTTP server requests.",
        "s",
        duration.as_secs_f64(),
        "http.response.status_code" = status_code as i64
    );
}

/// Records a request hyper rejected before the axum service layer saw it, such as a 431 for
/// oversized headers.
///
/// `start` is taken from before `serve_connection_with_upgrades`, so on a keep-alive connection
/// that already served valid requests the duration is inflated. These rejections almost always
/// occur on a connection's first request.
pub(crate) fn record_rejected_request(status_code: u16, start: Instant) {
    let elapsed = start.elapsed();
    record_bypassed_request(status_code, elapsed);

    // Give APM tools and Studio a span shaped like a normal router span. No distributed trace
    // context is available, because the headers never parsed.
    //
    // Enter the span before recording so OTel exporters derive a non-zero wall-clock duration
    // from on_enter/on_close. Dropping the guard at end of function closes the span.
    let entered = span_factory::create_router_rejection().entered();
    entered.record("http.response.status_code", status_code as i64);
    entered.record("apollo_private.duration_ns", elapsed.as_nanos() as i64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::config_new::instruments::HTTP_SERVER_REQUEST_DURATION_METRIC;

    /// The literal in `record_bypassed_request` has to match the instrument the router pipeline
    /// builds, or APM tools report the two populations separately.
    #[test]
    fn pipeline_bypass_metric_matches_router_instrument() {
        assert_eq!(
            HTTP_SERVER_REQUEST_DURATION_METRIC,
            "http.server.request.duration"
        );
    }

    #[tokio::test]
    async fn records_a_rejected_request() {
        async {
            record_rejected_request(431, Instant::now());
            assert_histogram_count!(
                "http.server.request.duration",
                1,
                "http.response.status_code" = 431i64
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn records_a_bypassed_request() {
        async {
            record_bypassed_request(503, Duration::from_millis(5));
            assert_histogram_count!(
                "http.server.request.duration",
                1,
                "http.response.status_code" = 503i64
            );
        }
        .with_metrics()
        .await;
    }
}
