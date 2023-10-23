use std::fmt::Display;
use std::fmt::{self};
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use lru::LruCache;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::redis::*;

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
        redis_urls: Option<Vec<url::Url>>,
        timeout: Option<Duration>,
        caller: &str,
    ) -> Self {
        Self {
            caller: caller.to_string(),
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
            redis: if let Some(urls) = redis_urls {
                match RedisCacheStorage::new(urls, None, timeout).await {
                    Err(e) => {
                        tracing::error!(
                            "could not open connection to Redis for {} caching: {:?}",
                            caller,
                            e
                        );
                        None
                    }
                    Ok(storage) => Some(storage),
                }
            } else {
                None
            },
        }
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
                .insert(RedisKey(key.clone()), RedisValue(value.clone()))
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

    pub(crate) async fn in_memory_keys(&self) -> Vec<K> {
        self.inner
            .lock()
            .await
            .iter()
            .map(|(k, _)| k.clone())
            .collect()
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
