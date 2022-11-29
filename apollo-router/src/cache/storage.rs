// This entire file is license key functionality

use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

use lru::LruCache;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;

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
    inner: Arc<Mutex<LruCache<K, V>>>,
    redis: Option<RedisCacheStorage>,
}

impl<K, V> CacheStorage<K, V>
where
    K: KeyType,
    V: ValueType,
{
    pub(crate) async fn new(
        max_capacity: usize,
        redis_urls: Option<Vec<String>>,
        caller: &str,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
            redis: if let Some(urls) = redis_urls {
                match RedisCacheStorage::new(urls).await {
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
        let mut guard = self.inner.lock().await;
        match guard.get(key) {
            Some(v) => Some(v.clone()),
            None => {
                if let Some(redis) = self.redis.as_ref() {
                    let inner_key = RedisKey(key.clone());
                    match redis.get::<K, V>(inner_key).await {
                        Some(v) => {
                            guard.put(key.clone(), v.0.clone());
                            Some(v.0)
                        }
                        None => None,
                    }
                } else {
                    None
                }
            }
        }
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        self.inner.lock().await.put(key.clone(), value.clone());

        if let Some(redis) = self.redis.as_ref() {
            redis.insert(RedisKey(key), RedisValue(value)).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}
