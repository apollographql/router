use std::fmt::Display;
use std::fmt::{self};
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use lru::LruCache;
use opentelemetry::metrics::MeterProvider;
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
    caller: &'static str,
    inner: Arc<Mutex<LruCache<K, V>>>,
    redis: Option<RedisCacheStorage>,
    cache_size: Arc<AtomicI64>,
    cache_estimated_storage: Arc<AtomicI64>,
    // It's OK for these to be mutexes as they are only initialized once
    cache_size_gauge: Arc<std::sync::Mutex<Option<ObservableGauge<i64>>>>,
    cache_estimated_storage_gauge: Arc<std::sync::Mutex<Option<ObservableGauge<i64>>>>,
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
        Ok(Self {
            cache_size_gauge: Default::default(),
            cache_estimated_storage_gauge: Default::default(),
            cache_size: Default::default(),
            cache_estimated_storage: Default::default(),
            caller,
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

    fn create_cache_size_gauge(&self) -> ObservableGauge<i64> {
        let meter: opentelemetry::metrics::Meter = metrics::meter_provider().meter(METER_NAME);
        let current_cache_size_for_gauge = self.cache_size.clone();
        let caller = self.caller;
        meter
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
            .init()
    }

    fn create_cache_estimated_storage_size_gauge(&self) -> ObservableGauge<i64> {
        let meter: opentelemetry::metrics::Meter = metrics::meter_provider().meter(METER_NAME);
        let cache_estimated_storage_for_gauge = self.cache_estimated_storage.clone();
        let caller = self.caller;
        let cache_estimated_storage_gauge = meter
            .i64_observable_gauge("apollo.router.cache.storage.estimated_size")
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
        cache_estimated_storage_gauge
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
        // Gauges MUST be created after the meter provider is initialized
        // This means that on reload we need a non-fallible way to recreate the gauges, hence this function.
        *self.cache_size_gauge.lock().expect("lock poisoned") =
            Some(self.create_cache_size_gauge());
        *self
            .cache_estimated_storage_gauge
            .lock()
            .expect("lock poisoned") = Some(self.create_cache_estimated_storage_size_gauge());
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
                "apollo_router_cache_size",
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
                "apollo_router_cache_size",
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
                "apollo_router_cache_size",
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
                "apollo_router_cache_size",
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
                "apollo_router_cache_size",
                1,
                "kind" = "test",
                "type" = "memory"
            );
        }
        .with_metrics()
        .await;
    }
}
