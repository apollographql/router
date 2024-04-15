use std::fmt::Display;
use std::fmt::{self};
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tower::BoxError;

use super::redis::*;
use crate::configuration::RedisCache;

pub(crate) trait KeyType:
    Clone + fmt::Debug + fmt::Display + Hash + Eq + Send + Sync
{
}
pub(crate) trait ValueType:
    Clone + fmt::Debug + Send + Sync + Serialize + DeserializeOwned
{
}

// Blanket implementation which satisfies the compiler
impl<K> KeyType for K
where
    K: Clone + fmt::Debug + fmt::Display + Hash + Eq + Send + Sync,
{
    // Nothing to implement, since K already supports the other traits.
    // It has the functions it needs already
}

// Blanket implementation which satisfies the compiler
impl<V> ValueType for V
where
    V: Clone + fmt::Debug + Send + Sync + Serialize + DeserializeOwned,
{
    // Nothing to implement, since V already supports the other traits.
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
}

impl<K, V> CacheStorage<K, V>
where
    K: KeyType,
    V: ValueType,
{
    pub(crate) async fn new(
        max_capacity: NonZeroUsize,
        config: Option<RedisCache>,
        caller: &str,
    ) -> Result<Self, BoxError> {
        Ok(Self {
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

    pub(crate) async fn get(&self, key: &K) -> Option<V> {
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
                    match redis.get::<K, V>(inner_key).await {
                        Some(v) => {
                            self.inner.lock().await.put(key.clone(), v.0.clone());

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

        let mut in_memory = self.inner.lock().await;
        in_memory.put(key, value);
        let size = in_memory.len() as u64;
        tracing::info!(
            value.apollo_router_cache_size = size,
            kind = %self.caller,
            storage = &tracing::field::display(CacheStorageName::Memory),
        );
    }

    pub(crate) async fn insert_in_memory(&self, key: K, value: V) {
        let mut in_memory = self.inner.lock().await;
        in_memory.put(key, value);
        let size = in_memory.len() as u64;
        tracing::info!(
            value.apollo_router_cache_size = size,
            kind = %self.caller,
            storage = &tracing::field::display(CacheStorageName::Memory),
        );
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
