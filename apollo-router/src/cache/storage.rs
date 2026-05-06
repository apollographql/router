use std::fmt::Display;
use std::fmt::{self};
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lru::LruCache;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use serde::Serialize;
use serde::de::DeserializeOwned;
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
    caller: &'static str,
    inner: Arc<Mutex<LruCache<K, V>>>,
    redis: Option<RedisCacheStorage>,
    cache_size: Arc<AtomicI64>,
    cache_estimated_storage: Arc<AtomicI64>,
    // It's OK for these to be mutexes as they are only initialized once
    cache_size_gauge: Arc<parking_lot::Mutex<Option<ObservableGauge<i64>>>>,
    cache_estimated_storage_gauge: Arc<parking_lot::Mutex<Option<ObservableGauge<i64>>>>,
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
        let maybe_redis_cache_storage = if let Some(config) = config {
            let required_to_start = config.required_to_start;
            let storage = match RedisCacheStorage::new(config, caller).await {
                Ok(storage) => Some(storage),
                Err(e) => {
                    tracing::error!(
                        cache = caller,
                        e,
                        "could not open connection to Redis for caching",
                    );
                    if required_to_start {
                        return Err(e);
                    }
                    // WARN: this is a terminal failure; we couldn't, for whatever reason noted in
                    // the error log, connect to redis--maybe it doesn't exist, maybe it's
                    // unreachable, who knows; but, this will prevent future commands from reaching
                    // redis
                    tracing::error!(
                        cache = caller,
                        e,
                        "terminal failure reached and all commands to Redis will fail",
                    );
                    None
                }
            };

            // NOTE: this populates the inner client pool, but failure doesn't represent a terminal
            // state unless the router is configred to require connections to start
            if let Some(storage) = storage.as_ref()
                && let Err(e) = storage.create_client_pool().await
            {
                tracing::error!(
                    cache = caller,
                    e,
                    "could not open connection to Redis for caching",
                );
                if required_to_start {
                    return Err(e);
                }
            }

            storage
        } else {
            None
        };

        Ok(Self {
            cache_size_gauge: Default::default(),
            cache_estimated_storage_gauge: Default::default(),
            cache_size: Default::default(),
            cache_estimated_storage: Default::default(),
            caller,
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
            redis: maybe_redis_cache_storage,
        })
    }

    pub(crate) fn new_in_memory(max_capacity: NonZeroUsize, caller: &'static str) -> Self {
        Self {
            cache_size_gauge: Default::default(),
            cache_estimated_storage_gauge: Default::default(),
            cache_size: Default::default(),
            cache_estimated_storage: Default::default(),
            caller,
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
            redis: None,
        }
    }

    fn create_cache_size_gauge(&self) -> ObservableGauge<i64> {
        let meter: opentelemetry::metrics::Meter = metrics::meter_provider().meter(METER_NAME);
        let current_cache_size_for_gauge = self.cache_size.clone();
        let caller = self.caller;
        meter
            .i64_observable_gauge("apollo.router.cache.size")
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
            .build()
    }

    fn create_cache_estimated_storage_size_gauge(&self) -> ObservableGauge<i64> {
        let meter: opentelemetry::metrics::Meter = metrics::meter_provider().meter(METER_NAME);
        let cache_estimated_storage_for_gauge = self.cache_estimated_storage.clone();
        let caller = self.caller;

        meter
            .i64_observable_gauge("apollo.router.cache.storage.estimated_size")
            .with_description("Estimated cache storage")
            .with_unit("bytes")
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
            .build()
    }

    /// Check the in-memory cache, then Redis on a miss. A Redis hit is promoted to the
    /// in-memory cache before returning. Emits `cache.hit.time` or `cache.miss.time` for
    /// each layer checked.
    ///
    /// `init_from_redis` is called on values freshly deserialized from Redis. Return `Err` to
    /// reject the entry and treat the lookup as a miss.
    pub(crate) async fn get(
        &self,
        key: &K,
        init_from_redis: impl FnMut(&mut V) -> Result<(), String>,
    ) -> Option<V> {
        if let Some(v) = self.get_in_memory(key).await {
            Some(v)
        } else if let Some(v) = self.get_from_redis(key, init_from_redis).await {
            self.insert_in_memory(key.clone(), v.clone()).await;
            Some(v)
        } else {
            None
        }
    }

    /// Check only the in-memory cache, bypassing Redis.
    /// Emits `cache.hit.time` on a hit and `cache.miss.time` on a miss.
    pub(crate) async fn get_in_memory(&self, key: &K) -> Option<V> {
        let instant = Instant::now();
        let res = self.inner.lock().await.get(key).cloned();
        if res.is_some() {
            self.record_cache_hit_duration(instant.elapsed(), CacheStorageName::Memory);
        } else {
            self.record_cache_miss_duration(instant.elapsed(), CacheStorageName::Memory);
        }
        res
    }

    /// For use by [`DeduplicatingCache`] only, as the in-memory fast path that avoids
    /// acquiring the wait_map mutex on warm-cache hits.
    ///
    /// Identical to [`CacheStorage::get_in_memory`] except it does not emit
    /// `cache.miss.time` on a miss — this check is an implementation detail of the
    /// deduplication layer, not a cache event visible to observers. On a miss, the caller
    /// falls through to `storage.get()`, which emits either `cache.hit.time` or
    /// `cache.miss.time` depending on whether another task inserted the value between
    /// this check and `storage.get()`'s in-memory re-check.
    pub(crate) async fn peek_in_memory(&self, key: &K) -> Option<V> {
        let instant = Instant::now();
        let res = self.inner.lock().await.get(key).cloned();
        if res.is_some() {
            self.record_cache_hit_duration(instant.elapsed(), CacheStorageName::Memory);
        }
        res
    }

    /// Check only Redis, returning the value without promoting it to the in-memory cache.
    /// Called by [`CacheStorage::get`] after an in-memory miss; promotion is the caller's
    /// responsibility.
    ///
    /// `init_from_redis` is called on values freshly deserialized from Redis. Return `Err` to
    /// reject the entry and treat the lookup as a miss.
    async fn get_from_redis(
        &self,
        key: &K,
        mut init_from_redis: impl FnMut(&mut V) -> Result<(), String>,
    ) -> Option<V> {
        let redis = self.redis.as_ref()?;

        let instant = Instant::now();
        let redis_value = redis
            .get(RedisKey(key.clone()))
            .await
            .ok()
            .and_then(|mut v| match init_from_redis(&mut v.0) {
                Ok(()) => Some(v),
                Err(e) => {
                    tracing::error!("Invalid value from Redis cache: {e}");
                    None
                }
            });
        if let Some(v) = redis_value {
            self.record_cache_hit_duration(instant.elapsed(), CacheStorageName::Redis);
            Some(v.0)
        } else {
            self.record_cache_miss_duration(instant.elapsed(), CacheStorageName::Redis);
            None
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
        let new_value_size = value.estimated_size().unwrap_or(0) as i64;

        let (old_value, length) = {
            let mut in_memory = self.inner.lock().await;
            (in_memory.push(key, value), in_memory.len())
        };

        let size_delta = match old_value {
            Some((_, old_value)) => {
                let old_value_size = old_value.estimated_size().unwrap_or(0) as i64;
                new_value_size - old_value_size
            }
            None => new_value_size,
        };
        self.cache_estimated_storage
            .fetch_add(size_delta, Ordering::SeqCst);

        self.cache_size.store(length as i64, Ordering::SeqCst);
    }

    pub(crate) fn in_memory_cache(&self) -> InMemoryCache<K, V> {
        self.inner.clone()
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub(crate) fn activate(&self) {
        // Gauges MUST be created after the meter provider is initialized.
        // This means that on reload we need a non-fallible way to recreate the gauges.
        *self.cache_size_gauge.lock() = Some(self.create_cache_size_gauge());
        *self.cache_estimated_storage_gauge.lock() =
            Some(self.create_cache_estimated_storage_size_gauge());

        // Also activate Redis metrics if present
        if let Some(redis) = &self.redis {
            redis.activate();
        }
    }

    fn record_cache_hit_duration(&self, duration: Duration, storage: CacheStorageName) {
        f64_histogram!(
            "apollo.router.cache.hit.time",
            "Time to get a value from the cache in seconds",
            duration.as_secs_f64(),
            kind = self.caller,
            storage = storage.to_string()
        );
    }

    fn record_cache_miss_duration(&self, duration: Duration, storage: CacheStorageName) {
        f64_histogram!(
            "apollo.router.cache.miss.time",
            "Time to check the cache for an uncached value in seconds",
            duration.as_secs_f64(),
            kind = self.caller,
            storage = storage.to_string()
        );
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

#[cfg(test)]
mod test {
    use std::num::NonZeroUsize;

    use crate::cache::estimate_size;
    use crate::cache::storage::CacheStorage;
    use crate::cache::storage::ValueType;
    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_metrics() {
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        struct Stuff {}
        impl ValueType for Stuff {
            fn estimated_size(&self) -> Option<usize> {
                Some(1)
            }
        }

        async {
            let cache: CacheStorage<String, Stuff> =
                CacheStorage::new(NonZeroUsize::new(10).unwrap(), None, "test")
                    .await
                    .unwrap();
            cache.activate();

            cache.insert("test".to_string(), Stuff {}).await;
            assert_gauge!(
                "apollo.router.cache.storage.estimated_size",
                1,
                "kind" = "test",
                "type" = "memory"
            );
            assert_gauge!(
                "apollo.router.cache.size",
                1,
                "kind" = "test",
                "type" = "memory"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_metrics_not_emitted_where_no_estimated_size() {
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        struct Stuff {}
        impl ValueType for Stuff {
            fn estimated_size(&self) -> Option<usize> {
                None
            }
        }

        async {
            let cache: CacheStorage<String, Stuff> =
                CacheStorage::new(NonZeroUsize::new(10).unwrap(), None, "test")
                    .await
                    .unwrap();
            cache.activate();

            cache.insert("test".to_string(), Stuff {}).await;
            // This metric won't exist
            assert_gauge!(
                "apollo.router.cache.size",
                0,
                "kind" = "test",
                "type" = "memory"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_metrics_eviction() {
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        struct Stuff {
            test: String,
        }
        impl ValueType for Stuff {
            fn estimated_size(&self) -> Option<usize> {
                Some(estimate_size(self))
            }
        }

        async {
            // note that the cache size is 1
            // so the second insert will always evict
            let cache: CacheStorage<String, Stuff> =
                CacheStorage::new(NonZeroUsize::new(1).unwrap(), None, "test")
                    .await
                    .unwrap();
            cache.activate();

            cache
                .insert(
                    "test".to_string(),
                    Stuff {
                        test: "test".to_string(),
                    },
                )
                .await;
            assert_gauge!(
                "apollo.router.cache.storage.estimated_size",
                28,
                "kind" = "test",
                "type" = "memory"
            );
            assert_gauge!(
                "apollo.router.cache.size",
                1,
                "kind" = "test",
                "type" = "memory"
            );

            // Insert something slightly larger
            cache
                .insert(
                    "test".to_string(),
                    Stuff {
                        test: "test_extended".to_string(),
                    },
                )
                .await;
            assert_gauge!(
                "apollo.router.cache.storage.estimated_size",
                37,
                "kind" = "test",
                "type" = "memory"
            );
            assert_gauge!(
                "apollo.router.cache.size",
                1,
                "kind" = "test",
                "type" = "memory"
            );

            // Even though this is a new cache entry, we should get back to where we initially were
            cache
                .insert(
                    "test2".to_string(),
                    Stuff {
                        test: "test".to_string(),
                    },
                )
                .await;
            assert_gauge!(
                "apollo.router.cache.storage.estimated_size",
                28,
                "kind" = "test",
                "type" = "memory"
            );
            assert_gauge!(
                "apollo.router.cache.size",
                1,
                "kind" = "test",
                "type" = "memory"
            );
        }
        .with_metrics()
        .await;
    }
}
