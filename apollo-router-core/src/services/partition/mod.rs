use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use quick_cache::sync::Cache;
use tower::Service;

/// A partition key type that can be used to partition requests.
///
/// This trait is automatically implemented for any type that satisfies the required bounds.
/// The partition key is used to determine which cached service instance should handle a request.
pub trait PartitionKey: Clone + Hash + Eq + Send + Sync + 'static {}

impl<T> PartitionKey for T where T: Clone + Hash + Eq + Send + Sync + 'static {}

/// A service that partitions requests based on a partition key and caches the resulting services.
///
/// This service acts as a router that:
/// 1. Extracts a partition key from each incoming request
/// 2. Looks up or creates a service instance for that partition key
/// 3. Forwards the request to the appropriate service instance
///
/// # Service Caching
///
/// Services are cached by their partition key using `quick_cache`. Once created, a service
/// instance is reused for all subsequent requests with the same partition key. This provides
/// efficient request routing without the overhead of recreating services.
///
/// # Backpressure Handling
///
/// **Important**: This service does **not** preserve backpressure from downstream services.
///
/// The partition service always reports itself as ready (`poll_ready` returns `Ready(Ok(()))`),
/// regardless of the readiness state of cached services. This design choice is made because:
///
/// 1. **Multiple partitions**: Different partition keys may map to different service instances
///    with varying readiness states. There's no single readiness state to report.
/// 2. **Dynamic routing**: The target service is determined at call time, not at ready time.
/// 3. **Caching overhead**: Checking readiness of all cached services would be expensive.
///
/// ## Implications
///
/// - **Load shedding**: Upstream services cannot rely on backpressure signals for load control
/// - **Resource management**: Individual partition services must implement their own load shedding
/// - **Circuit breaking**: Consider implementing circuit breakers or rate limiting at the partition level
/// - **Memory usage**: Cached services may accumulate requests without upstream awareness
///
/// ## Recommendations
///
/// For proper load management in systems using this service:
///
/// - Implement timeouts on service calls
/// - Add circuit breakers or rate limiting within partition services
/// - Monitor resource usage at the partition level
/// - Consider implementing admission control based on system resources rather than backpressure
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::services::partition::PartitionService;
///
/// // Create the partition service with closures (recommended)
/// let mut partition_service = PartitionService::new(
///     |request| request.user_id.clone(),           // Partition key extractor
///     |user_id| MyUserService::new(user_id),       // Service factory
/// );
///
/// // Use the service
/// let response = partition_service.call(request).await?;
/// ```
///
pub struct PartitionService<K, F, Req, S> {
    partition_fn: Arc<dyn Fn(&Req) -> K + Send + Sync>,
    make_service: F,
    cache: Arc<Cache<K, S>>,
    _phantom: std::marker::PhantomData<Req>,
}

impl<K, F, Req, S> Clone for PartitionService<K, F, Req, S>
where
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            partition_fn: self.partition_fn.clone(),
            make_service: self.make_service.clone(),
            cache: self.cache.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl PartitionService<(), (), (), ()> {
    /// Create a new partition service with default cache settings.
    ///
    /// # Arguments
    ///
    /// * `partition_fn` - A function that extracts a partition key from requests
    /// * `make_service` - A closure for creating service instances for each partition
    ///
    /// The default cache size is 1000 entries. Use [`with_cache_size`](Self::with_cache_size)
    /// if you need a different cache size.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let service = PartitionService::new(
    ///     |req| req.tenant_id.clone(),
    ///     |tenant_id| MyService::new(tenant_id)
    /// );
    /// ```
    pub fn new<K, Req, P, F, S>(partition_fn: P, make_service: F) -> PartitionService<K, F, Req, S>
    where
        P: Fn(&Req) -> K + Send + Sync + 'static,
        F: Fn(K) -> S + Clone + Send + Sync + 'static,
        S: Service<Req> + Send + Clone + 'static,
        S::Future: Send + 'static,
        K: PartitionKey,
        Req: Send + 'static,
    {
        PartitionService {
            partition_fn: Arc::new(partition_fn),
            make_service,
            cache: Arc::new(Cache::new(1000)), // Default cache size of 1000
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create a new partition service with a specific cache size.
    ///
    /// # Arguments
    ///
    /// * `partition_fn` - A function that extracts a partition key from requests
    /// * `make_service` - A closure for creating service instances for each partition
    /// * `cache_size` - Maximum number of services to cache (approximately)
    ///
    /// # Cache Size Considerations
    ///
    /// - **Too small**: Frequent service recreation, higher latency
    /// - **Too large**: Higher memory usage, potential for stale services
    /// - **Rule of thumb**: Set to ~2x the expected number of active partitions
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let service = PartitionService::with_cache_size(
    ///     |req| req.tenant_id.clone(),
    ///     |tenant_id| MyService::new(tenant_id),
    ///     500  // Cache up to 500 tenant services
    /// );
    /// ```
    pub fn with_cache_size<K, Req, P, F, S>(
        partition_fn: P,
        make_service: F,
        cache_size: usize,
    ) -> PartitionService<K, F, Req, S>
    where
        P: Fn(&Req) -> K + Send + Sync + 'static,
        F: Fn(K) -> S + Clone + Send + Sync + 'static,
        S: Service<Req> + Send + Clone + 'static,
        S::Future: Send + 'static,
        K: PartitionKey,
        Req: Send + 'static,
    {
        PartitionService {
            partition_fn: Arc::new(partition_fn),
            make_service,
            cache: Arc::new(Cache::new(cache_size)),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<K, F, Req, S> Service<Req> for PartitionService<K, F, Req, S>
where
    K: PartitionKey,
    F: Fn(K) -> S + Clone + Send + Sync + 'static,
    S: Service<Req> + Send + Clone + 'static,
    S::Future: Send + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Always ready - backpressure is not preserved across partitions.
        // See the struct-level documentation for details on backpressure handling.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Req) -> Self::Future {
        // Extract partition key from the request
        let partition = (self.partition_fn)(&req);
        let make_service = self.make_service.clone();

        // Get or create the service outside the async block to avoid holding
        // the cache lock across await points
        let service = self
            .cache
            .get_or_insert_with(&partition, || {
                let new_service = make_service(partition.clone());
                Ok::<_, ()>(new_service)
            })
            .unwrap();

        // Clone the service to avoid borrowing issues in the async block
        let service_instance = service.clone();

        Box::pin(async move {
            // Use oneshot to properly handle the Service protocol (ready + call)
            // This ensures the service is ready before calling it
            use tower::ServiceExt;
            service_instance.oneshot(req).await
        })
    }
}

#[cfg(test)]
mod tests;
