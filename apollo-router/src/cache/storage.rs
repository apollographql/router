use std::fmt::Display;
use std::fmt::{self};
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use lru::LruCache;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::metrics::Meter;
use opentelemetry_api::metrics::ObservableGauge;
use opentelemetry_api::metrics::Unit;
use opentelemetry_api::KeyValue;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tower::BoxError;

use super::redis::*;
use crate::configuration::RedisCache;
use crate::metrics;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;

pub(crate) trait KeyType:
    Clone + fmt::Debug + fmt::Display + Hash + Eq + Send + Sync
{
}
pub(crate) trait ValueType:
    Clone + fmt::Debug + Send + Sync + Serialize + DeserializeOwned
{
    /// Returns an estimated size of the cache entry in bytes.
    fn estimated_size(&self) -> Option<usize> {
        None
    }
}

// Blanket implementation which satisfies the compiler
impl<K> KeyType for K
where
    K: Clone + fmt::Debug + fmt::Display + Hash + Eq + Send + Sync,
{
    // Nothing to implement, since K already supports the other traits.
    // It has the functions it needs already
}

pub(crate) type InMemoryCache<K, V> = Arc<Mutex<LruCache<K, V>>>;

// placeholder storage module
//
// this will be replaced by the multi level (in memory + redis/memcached) once we find
// a suitable implementation.
#[derive(Clone)]
pub(crate) struct CacheStorage<K: KeyType, V: ValueType> {
    caller: String,
    inner: Arc<Mutex<LruCache<K, V>>>,
    redis: Option<RedisCacheStorage>,
    cache_size: Arc<AtomicI64>,
    cache_estimated_storage: Arc<AtomicI64>,
    _cache_size_gauge: ObservableGauge<i64>,
    _cache_estimated_storage_gauge: ObservableGauge<i64>,
}

impl<K, V> CacheStorage<K, V>
where
    K: KeyType,
    V: ValueType,
{
    pub(crate) async fn new(
        max_capacity: NonZeroUsize,
        config: Option<RedisCache>,
        caller: &'static str,
    ) -> Result<Self, BoxError> {
        // Because calculating the cache size is expensive we do this as we go rather than iterating. This means storing the values for the gauges
        let meter: opentelemetry::metrics::Meter = metrics::meter_provider().meter(METER_NAME);
        let (cache_size, cache_size_gauge) = Self::create_cache_size_gauge(&meter, caller);
        let (cache_estimated_storage, cache_estimated_storage_gauge) =
            Self::create_cache_estimated_storage_size_gauge(&meter, caller);

        Ok(Self {
            _cache_size_gauge: cache_size_gauge,
            _cache_estimated_storage_gauge: cache_estimated_storage_gauge,
            cache_size,
            cache_estimated_storage,
            caller: caller.to_string(),
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
            redis: if let Some(config) = config {
                let required_to_start = config.required_to_start;
                match RedisCacheStorage::new(config).await {
                    Err(e) => {
                        tracing::error!(
                            cache = caller,
                            e,
                            "could not open connection to Redis for caching",
                        );
                        if required_to_start {
                            return Err(e);
                        }
                        None
                    }
                    Ok(storage) => Some(storage),
                }
            } else {
                None
            },
        })
    }

    fn create_cache_size_gauge(
        meter: &Meter,
        caller: &'static str,
    ) -> (Arc<AtomicI64>, ObservableGauge<i64>) {
        let current_cache_size = Arc::new(AtomicI64::new(0));
        let current_cache_size_for_gauge = current_cache_size.clone();
        let cache_size_gauge = meter
            // TODO move to dot naming convention
            .i64_observable_gauge("apollo_router_cache_size")
            .with_description("Cache size")
            .with_callback(move |i| {
                i.observe(
                    current_cache_size_for_gauge.load(Ordering::SeqCst),
                    &[
                        KeyValue::new("kind", caller),
                        KeyValue::new("type", "memory"),
                    ],
                )
            })
            .init();
        (current_cache_size, cache_size_gauge)
    }

    fn create_cache_estimated_storage_size_gauge(
        meter: &Meter,
        caller: &'static str,
    ) -> (Arc<AtomicI64>, ObservableGauge<i64>) {
        let cache_estimated_storage = Arc::new(AtomicI64::new(0));
        let cache_estimated_storage_for_gauge = cache_estimated_storage.clone();
        let cache_estimated_storage_gauge = meter
            .i64_observable_gauge("apollo.router.cache.estimated.storage.size")
            .with_description("Estimated cache storage")
            .with_unit(Unit::new("bytes"))
            .with_callback(move |i| {
                // If there's no storage then don't bother updating the gauge
                let value = cache_estimated_storage_for_gauge.load(Ordering::SeqCst);
                if value > 0 {
                    i.observe(
                        cache_estimated_storage_for_gauge.load(Ordering::SeqCst),
                        &[
                            KeyValue::new("kind", caller),
                            KeyValue::new("type", "memory"),
                        ],
                    )
                }
            })
            .init();
        (cache_estimated_storage, cache_estimated_storage_gauge)
    }

    /// `init_from_redis` is called with values newly deserialized from Redis cache
    /// if an error is returned, the value is ignored and considered a cache miss.
    pub(crate) async fn get(
        &self,
        key: &K,
        mut init_from_redis: impl FnMut(&mut V) -> Result<(), String>,
    ) -> Option<V> {
        let instant_memory = Instant::now();
        let res = self.inner.lock().await.get(key).cloned();

        match res {
            Some(v) => {
                tracing::info!(
                    monotonic_counter.apollo_router_cache_hit_count = 1u64,
                    kind = %self.caller,
                    storage = &tracing::field::display(CacheStorageName::Memory),
                );
                let duration = instant_memory.elapsed().as_secs_f64();
                tracing::info!(
                    histogram.apollo_router_cache_hit_time = duration,
                    kind = %self.caller,
                    storage = &tracing::field::display(CacheStorageName::Memory),
                );
                Some(v)
            }
            None => {
                let duration = instant_memory.elapsed().as_secs_f64();
                tracing::info!(
                    histogram.apollo_router_cache_miss_time = duration,
                    kind = %self.caller,
                    storage = &tracing::field::display(CacheStorageName::Memory),
                );
                tracing::info!(
                    monotonic_counter.apollo_router_cache_miss_count = 1u64,
                    kind = %self.caller,
                    storage = &tracing::field::display(CacheStorageName::Memory),
                );

                let instant_redis = Instant::now();
                if let Some(redis) = self.redis.as_ref() {
                    let inner_key = RedisKey(key.clone());
                    let redis_value =
                        redis
                            .get::<K, V>(inner_key)
                            .await
                            .and_then(|mut v| match init_from_redis(&mut v.0) {
                                Ok(()) => Some(v),
                                Err(e) => {
                                    tracing::error!("Invalid value from Redis cache: {e}");
                                    None
                                }
                            });
                    match redis_value {
                        Some(v) => {
                            self.insert_in_memory(key.clone(), v.0.clone()).await;

                            tracing::info!(
                                monotonic_counter.apollo_router_cache_hit_count = 1u64,
                                kind = %self.caller,
                                storage = &tracing::field::display(CacheStorageName::Redis),
                            );
                            let duration = instant_redis.elapsed().as_secs_f64();
                            tracing::info!(
                                histogram.apollo_router_cache_hit_time = duration,
                                kind = %self.caller,
                                storage = &tracing::field::display(CacheStorageName::Redis),
                            );
                            Some(v.0)
                        }
                        None => {
                            tracing::info!(
                                monotonic_counter.apollo_router_cache_miss_count = 1u64,
                                kind = %self.caller,
                                storage = &tracing::field::display(CacheStorageName::Redis),
                            );
                            let duration = instant_redis.elapsed().as_secs_f64();
                            tracing::info!(
                                histogram.apollo_router_cache_miss_time = duration,
                                kind = %self.caller,
                                storage = &tracing::field::display(CacheStorageName::Redis),
                            );
                            None
                        }
                    }
                } else {
                    None
                }
            }
        }
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        if let Some(redis) = self.redis.as_ref() {
            redis
                .insert(RedisKey(key.clone()), RedisValue(value.clone()), None)
                .await;
        }

        self.insert_in_memory(key, value).await;
    }

    pub(crate) async fn insert_in_memory(&self, key: K, value: V)
    where
        V: ValueType,
    {
        // Update the cache size and estimated storage size
        // This is cheaper than trying to estimate the cache storage size by iterating over the cache
        self.cache_estimated_storage
            .fetch_add(value.estimated_size().unwrap_or(0) as i64, Ordering::SeqCst);
        let mut in_memory = self.inner.lock().await;
        if let Some((_, v)) = in_memory.push(key.clone(), value.clone()) {
            self.cache_estimated_storage
                .fetch_sub(v.estimated_size().unwrap_or(0) as i64, Ordering::SeqCst);
        }
        self.cache_size
            .store(in_memory.len() as i64, Ordering::SeqCst);
    }

    pub(crate) fn in_memory_cache(&self) -> InMemoryCache<K, V> {
        self.inner.clone()
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

enum CacheStorageName {
    Redis,
    Memory,
}

impl Display for CacheStorageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheStorageName::Redis => write!(f, "redis"),
            CacheStorageName::Memory => write!(f, "memory"),
        }
    }
}

impl ValueType for String {
    fn estimated_size(&self) -> Option<usize> {
        Some(self.len())
    }
}

impl ValueType for crate::graphql::Response {
    fn estimated_size(&self) -> Option<usize> {
        None
    }
}

impl ValueType for usize {
    fn estimated_size(&self) -> Option<usize> {
        Some(std::mem::size_of::<usize>())
    }
}
