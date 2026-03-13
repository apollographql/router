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

use crate::allocator::WithMemoryTracking;
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
    let agg_clone = aggregation.clone();
    builder.with_view(MeterProviderType::Public, move |instrument: &Instrument| {
        if instrument.name() == "apollo.router.request.memory" {
            Some(
                Stream::builder()
                    .with_aggregation(agg_clone.clone())
                    .build()
                    .expect("Failed to create stream for apollo.router.request.memory metric"),
            )
        } else {
            None
        }
    });

    // Register view for query planner memory metric
    builder.with_view(MeterProviderType::Public, move |instrument: &Instrument| {
        if instrument.name() == "apollo.router.query_planner.memory" {
            Some(
                Stream::builder()
                    .with_aggregation(aggregation.clone())
                    .build()
                    .expect(
                        "Failed to create stream for apollo.router.query_planner.memory metric",
                    ),
            )
        } else {
            None
        }
    });
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
        let fut = self.inner.call(req);
        Box::pin(
            async move {
                let result = fut.await;

                // Record allocation metrics if stats are available
                #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
                if let Some(stats) = crate::allocator::current() {
                    record_metrics(&stats);
                }

                result
            }
            .with_memory_tracking("router.request"),
        )
    }
}

/// Record allocation metrics for a specific context.
#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
fn record_metrics(stats: &crate::allocator::AllocationStats) {
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

#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix, test))]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use tower::ServiceExt;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::services::router;

    #[tokio::test]
    async fn test_allocation_metrics_layer() {
        async {
            let allocated_bytes = Arc::new(AtomicU64::new(0));
            let allocated_bytes_clone = allocated_bytes.clone();

            // Create a simple service that allocates memory
            let service = tower::service_fn(move |_req: router::Request| {
                let allocated_bytes_clone = allocated_bytes_clone.clone();
                async move {
                    // Allocate some memory during request processing
                    let _v = Vec::<u8>::with_capacity(10000);
                    let result =
                        Ok::<_, tower::BoxError>(router::Response::fake_builder().build().unwrap());

                    allocated_bytes_clone.as_ref().store(
                        crate::allocator::current()
                            .expect("stats should be set")
                            .bytes_allocated() as u64,
                        Ordering::Relaxed,
                    );

                    result
                }
            });

            // Wrap with allocation metrics layer
            let layer = AllocationMetricsLayer::new();
            let mut service = layer.layer(service);

            // Make a request
            let request = router::Request::fake_builder().build().unwrap();
            let _response = service.ready().await.unwrap().call(request).await.unwrap();

            assert!(allocated_bytes.load(Ordering::Relaxed) > 10000);

            // Verify metrics were recorded
            assert_histogram_sum!(
                "apollo.router.request.memory",
                // Varies depending on platform
                allocated_bytes.load(Ordering::Relaxed),
                "allocation.type" = "allocated",
                "context" = "router.request"
            );

            assert_histogram_sum!(
                "apollo.router.request.memory",
                10000,
                "allocation.type" = "deallocated",
                "context" = "router.request"
            );

            assert_histogram_sum!(
                "apollo.router.request.memory",
                0,
                "allocation.type" = "zeroed",
                "context" = "router.request"
            );

            assert_histogram_sum!(
                "apollo.router.request.memory",
                0,
                "allocation.type" = "reallocated",
                "context" = "router.request"
            );
        }
        .with_metrics()
        .await;
    }
}
