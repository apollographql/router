use std::future::Future;
use std::hash::Hash;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use quick_cache::sync::Cache;
use tower::{Layer, Service};
use tower::load_shed::error::Overloaded;
pub type ArcError = Arc<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// Cache operation failed
    #[error("Cache operation failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_CACHE_OPERATION_ERROR),
        help("Check cache configuration and system resources")
    )]
    CacheOperationError {
        #[extension("cacheMessage")]
        message: String,
    },
}

/// A generic caching layer that can cache successful responses and specific error types.
///
/// This layer provides intelligent caching with configurable key extraction and selective
/// error caching. It uses Arc for efficient storage, making cache hits extremely cheap
/// as they only require Arc pointer cloning.
///
/// # Key Features
///
/// - **Selective Error Caching**: Configurable predicate determines which errors to cache
/// - **Arc-Based Storage**: Zero-copy cache hits through Arc pointer cloning
/// - **Clock-PRO Eviction**: Uses quick_cache's optimal eviction algorithm
/// - **Load Shedding Protection**: Never caches transient Overloaded errors
/// - **Type Safety**: Strongly typed key extraction and error predicates
///
/// # Type Parameters
///
/// * `Req` - The request type for the service
/// * `Resp` - The response type for the service
/// * `K` - The cache key type (must implement Hash + Eq + Clone + Send + Sync)
/// * `F` - The key extraction function type `Fn(&Req) -> K`
/// * `P` - The error predicate function type `Fn(&ArcError) -> bool`
///
/// # Usage Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::cache::{CacheLayer, ArcError};
/// use apollo_router_core::services::query_parse::{Request as QueryParseRequest, Response as QueryParseResponse, Error as QueryParseError};
///
/// # fn example() {
/// // Create a cache layer for query parsing
/// let cache_layer: CacheLayer<QueryParseRequest, QueryParseResponse, String, _, _> = CacheLayer::new(
///     1000, // Cache capacity
///     |req: &QueryParseRequest| req.query.clone(), // Extract query string as key
///     |err: &ArcError| {
///         // Cache parse errors but not other error types
///         err.is::<QueryParseError>()
///     }
/// );
/// # }
/// ```
///
/// # Cache Behavior
///
/// - **Successful Responses**: Cached as `Arc<Resp>` for zero-copy hits
/// - **Selective Error Caching**: Only errors matching the predicate are cached
/// - **Overloaded Error Protection**: `tower::load_shed::Overloaded` errors are never cached
/// - **Efficient Hits**: Cache hits only clone Arc pointers (extremely fast)
/// - **Smart Eviction**: Uses Clock-PRO algorithm for optimal cache hit rates
/// - **Memory Efficient**: Shares data between cache entries and active responses
#[derive(Clone, Debug)]
pub struct CacheLayer<Req, Resp, K, F, P> {
    cache: Arc<Cache<K, Result<Arc<Resp>, ArcError>>>,
    key_extractor: F,
    error_predicate: P,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<Req, Resp, K, F, P> CacheLayer<Req, Resp, K, F, P>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    Resp: Send + Sync + 'static,
    F: Fn(&Req) -> K + Clone + Send + Sync + 'static,
    P: Fn(&ArcError) -> bool + Clone + Send + Sync + 'static,
{
    /// Creates a new caching layer with the specified capacity, key extraction function,
    /// and error predicate for determining which errors to cache.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of entries to cache
    /// * `key_extractor` - Function to extract cache key from requests
    /// * `error_predicate` - Function that determines which errors to cache (evaluated after excluding Overloaded errors)
    pub fn new(capacity: usize, key_extractor: F, error_predicate: P) -> Self {
        Self {
            cache: Arc::new(Cache::new(capacity)),
            key_extractor,
            error_predicate,
            _phantom: PhantomData,
        }
    }

    /// Creates a new caching layer with custom cache options, key extraction function,
    /// and error predicate for determining which errors to cache.
    ///
    /// # Arguments
    ///
    /// * `cache` - Pre-configured quick_cache instance
    /// * `key_extractor` - Function to extract cache key from requests
    /// * `error_predicate` - Function that determines which errors to cache (evaluated after excluding Overloaded errors)
    pub fn with_cache(
        cache: Cache<K, Result<Arc<Resp>, ArcError>>,
        key_extractor: F,
        error_predicate: P,
    ) -> Self {
        Self {
            cache: Arc::new(cache),
            key_extractor,
            error_predicate,
            _phantom: PhantomData,
        }
    }
}

impl<S, Req, Resp, K, F, P> Layer<S> for CacheLayer<Req, Resp, K, F, P>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    Resp: Send + Sync + 'static,
    F: Fn(&Req) -> K + Clone + Send + Sync + 'static,
    P: Fn(&ArcError) -> bool + Clone + Send + Sync + 'static,
{
    type Service = CacheService<S, Req, Resp, K, F, P>;

    fn layer(&self, inner: S) -> Self::Service {
        CacheService {
            inner,
            cache: self.cache.clone(),
            key_extractor: self.key_extractor.clone(),
            error_predicate: self.error_predicate.clone(),
            _phantom: PhantomData,
        }
    }
}

/// The caching service that wraps an inner service and provides caching functionality
#[derive(Debug)]
pub struct CacheService<S, Req, Resp, K, F, P> {
    inner: S,
    cache: Arc<Cache<K, Result<Arc<Resp>, ArcError>>>,
    key_extractor: F,
    error_predicate: P,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<S, Req, Resp, K, F, P> Clone for CacheService<S, Req, Resp, K, F, P>
where
    S: Clone,
    F: Clone,
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            cache: Arc::clone(&self.cache),
            key_extractor: self.key_extractor.clone(),
            error_predicate: self.error_predicate.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<S, Req, Resp, K, F, P> Service<Req> for CacheService<S, Req, Resp, K, F, P>
where
    S: Service<Req, Response = Resp> + Send + 'static,
    S::Error: Into<ArcError> + Send + 'static,
    S::Future: Send + 'static,
    Req: Send + 'static,
    Resp: Send + Sync + 'static,
    K: Hash + Eq + Clone + Send + Sync + 'static,
    F: Fn(&Req) -> K + Clone + Send + Sync + 'static,
    P: Fn(&ArcError) -> bool + Clone + Send + Sync + 'static,
{
    type Response = Arc<Resp>;
    type Error = ArcError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        // Extract cache key
        let cache_key = (self.key_extractor)(&req);
        let cache = Arc::clone(&self.cache);

        // Check cache first - if hit, just clone the Result (cheap Arc cloning)
        if let Some(cached_result) = cache.get(&cache_key) {
            return Box::pin(async move { cached_result });
        }

        // Call inner service
        let future = self.inner.call(req);
        let cache_for_insert = Arc::clone(&cache);
        let key_for_insert = cache_key.clone();
        let error_predicate = self.error_predicate.clone();

        Box::pin(async move {
            match future.await {
                Ok(response) => {
                    // Cache successful response and return
                    let arc_resp = Arc::new(response);
                    cache_for_insert.insert(key_for_insert, Ok(arc_resp.clone()));
                    Ok(arc_resp)
                }
                Err(err) => {
                    let arc_err = err.into();
                    
                    // Never cache Overloaded errors as they are transient
                    if arc_err.is::<Overloaded>() {
                        return Err(arc_err);
                    }
                    
                    let should_cache = error_predicate(&arc_err);

                    // Try to extract cacheable error
                    if should_cache {
                        // Cache the error and return
                        cache_for_insert.insert(key_for_insert, Err(arc_err.clone()));
                        Err(arc_err)
                    } else {
                        // Return original error without caching
                        Err(arc_err)
                    }
                }
            }
        })
    }
}

/// Convenience function to create a cache layer for query parsing
///
/// This is a specialized version that uses string-based cache keys and caches
/// specific parse error types by consuming the BoxError.
pub fn query_parse_cache<Req, Resp>(
    capacity: usize,
    key_extractor: impl Fn(&Req) -> String + Clone + Send + Sync + 'static,
    error_predicate: impl Fn(&ArcError) -> bool + Clone + Send + Sync + 'static,
) -> CacheLayer<
    Req,
    Resp,
    String,
    impl Fn(&Req) -> String + Clone + Send + Sync + 'static,
    impl Fn(&ArcError) -> bool + Clone + Send + Sync + 'static,
>
where
    Req: Send + 'static,
    Resp: Send + Sync + 'static,
{
    CacheLayer::new(capacity, key_extractor, error_predicate)
}

#[cfg(test)]
mod tests;
