//! Memory allocation tracking metrics for router requests.
//!
//! This module provides a Tower layer that wraps router requests with memory tracking,
//! measuring bytes allocated, deallocated, zeroed, and reallocated during request processing.

use std::task::Context;
use std::task::Poll;

use opentelemetry_sdk::metrics::Aggregation;
use opentelemetry_sdk::metrics::Instrument;
use opentelemetry_sdk::metrics::Stream;
use tower::Layer;
use tower::Service;

use crate::allocator::AllocationStats;
use crate::allocator::with_memory_tracking;
use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::reload::metrics::MetricsBuilder;
use crate::services::router;

/// Memory allocation histogram buckets: 1KB, 10KB, 100KB, 1MB, 10MB, 100MB
const MEMORY_BUCKETS: &[f64] = &[
    1_000.0,       // 1KB
    10_000.0,      // 10KB
    100_000.0,     // 100KB
    1_000_000.0,   // 1MB
    10_000_000.0,  // 10MB
    100_000_000.0, // 100MB
];

/// Register memory allocation metric views with custom bucket boundaries.
pub(crate) fn register_memory_allocation_views(builder: &mut MetricsBuilder) {
    // Create aggregation with memory-specific buckets
    let aggregation = Aggregation::ExplicitBucketHistogram {
        boundaries: MEMORY_BUCKETS.to_vec(),
        record_min_max: true,
    };

    // Register view for router request memory metric
    let request_view = opentelemetry_sdk::metrics::new_view(
        Instrument::new().name("apollo.router.request.memory"),
        Stream::new().aggregation(aggregation.clone()),
    )
    .unwrap();
    builder.with_view(MeterProviderType::Public, Box::new(request_view));

    // Register view for query planner memory metric
    let query_planner_view = opentelemetry_sdk::metrics::new_view(
        Instrument::new().name("apollo.router.query_planner.memory"),
        Stream::new().aggregation(aggregation),
    )
    .unwrap();
    builder.with_view(MeterProviderType::Public, Box::new(query_planner_view));
}

/// Tower layer that adds memory allocation tracking to router requests.
#[derive(Clone)]
pub(crate) struct AllocationMetricsLayer;

impl AllocationMetricsLayer {
    /// Create a new allocation metrics layer.
    pub(crate) fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for AllocationMetricsLayer {
    type Service = AllocationMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AllocationMetricsService { inner }
    }
}

/// Tower service that tracks memory allocations for each router request.
#[derive(Clone)]
pub(crate) struct AllocationMetricsService<S> {
    inner: S,
}

impl<S> Service<router::Request> for AllocationMetricsService<S>
where
    S: Service<router::Request, Response = router::Response> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        with_memory_tracking("router.request", || {
            let fut = self.inner.call(req);
            Box::pin(async move {
                // Everything within this future should be tracked
                crate::allocator::TRACKING_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
                let result = fut.await;
                crate::allocator::TRACKING_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);

                // Record allocation metrics if stats are available
                if let Some(stats) = crate::allocator::current() {
                    record_metrics(&stats);
                }

                result
            })
        })
    }
}

/// Record allocation metrics for a specific context.
fn record_metrics(stats: &AllocationStats) {
    let bytes_allocated = stats.bytes_allocated() as u64;
    let bytes_deallocated = stats.bytes_deallocated() as u64;
    let bytes_zeroed = stats.bytes_zeroed() as u64;
    let bytes_reallocated = stats.bytes_reallocated() as u64;
    let context_name = stats.name();

    // Record total bytes allocated
    u64_histogram_with_unit!(
        "apollo.router.request.memory",
        "Memory allocated during request processing",
        "By",
        bytes_allocated,
        allocation.type = "allocated",
        context = context_name
    );

    // Record bytes deallocated
    u64_histogram_with_unit!(
        "apollo.router.request.memory",
        "Memory allocated during request processing",
        "By",
        bytes_deallocated,
        allocation.type = "deallocated",
        context = context_name
    );

    // Record bytes zeroed
    u64_histogram_with_unit!(
        "apollo.router.request.memory",
        "Memory allocated during request processing",
        "By",
        bytes_zeroed,
        allocation.type = "zeroed",
        context = context_name
    );

    // Record bytes reallocated
    u64_histogram_with_unit!(
        "apollo.router.request.memory",
        "Memory allocated during request processing",
        "By",
        bytes_reallocated,
        allocation.type = "reallocated",
        context = context_name
    );
}

#[cfg(test)]
mod tests {
    use tower::ServiceExt;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::services::router;

    #[tokio::test]
    async fn test_allocation_metrics_layer() {
        async {
            // Create a simple service that allocates memory
            let service = tower::service_fn(|_req: router::Request| async {
                // Allocate some memory during request processing
                let _v = Vec::<u8>::with_capacity(10000);
                Ok::<_, tower::BoxError>(router::Response::fake_builder().build().unwrap())
            });

            // Wrap with allocation metrics layer
            let layer = AllocationMetricsLayer::new();
            let mut service = layer.layer(service);

            // Make a request
            let request = router::Request::fake_builder().build().unwrap();
            let _response = service.ready().await.unwrap().call(request).await.unwrap();

            // Verify metrics were recorded
            // Note: We can't easily assert on histogram values, but the test verifies
            // the layer compiles and runs without errors
        }
        .with_metrics()
        .await;
    }
}
