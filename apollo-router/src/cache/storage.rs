use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

use lru::LruCache;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;

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

#[cfg(feature = "experimental_cache")]
use redis_storage::*;

// placeholder storage module
//
// this will be replaced by the multi level (in memory + redis/memcached) once we find
// a suitable implementation.
#[derive(Clone)]
pub(crate) struct CacheStorage<K: KeyType, V: ValueType> {
    inner: Arc<Mutex<LruCache<K, V>>>,
    #[cfg(feature = "experimental_cache")]
    redis: Option<RedisCacheStorage>,
}

impl<K, V> CacheStorage<K, V>
where
    K: KeyType,
    V: ValueType,
{
    pub(crate) async fn new(max_capacity: usize, _redis_urls: Option<Vec<String>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
            #[cfg(feature = "experimental_cache")]
            redis: if let Some(urls) = _redis_urls {
                Some(RedisCacheStorage::new(urls).await)
            } else {
                None
            },
        }
    }

    pub(crate) async fn get(&self, key: &K) -> Option<V> {
        let mut guard = self.inner.lock().await;
        match guard.get(key) {
            Some(v) => Some(v.clone()),
            #[cfg(feature = "experimental_cache")]
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
            #[cfg(not(feature = "experimental_cache"))]
            None => None,
        }
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        self.inner.lock().await.put(key.clone(), value.clone());

        #[cfg(feature = "experimental_cache")]
        if let Some(redis) = self.redis.as_ref() {
            redis.insert(RedisKey(key), RedisValue(value)).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[cfg(feature = "experimental_cache")]
mod redis_storage {
    use std::fmt;
    use std::sync::Arc;

    use redis::AsyncCommands;
    use redis::FromRedisValue;
    use redis::RedisResult;
    use redis::RedisWrite;
    use redis::ToRedisArgs;
    use redis_cluster_async::Client;
    use redis_cluster_async::Connection;
    use tokio::sync::Mutex;

    use super::KeyType;
    use super::ValueType;

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    pub(crate) struct RedisKey<K>(pub(crate) K)
    where
        K: KeyType;

    #[derive(Clone, Debug)]
    pub(crate) struct RedisValue<V>(pub(crate) V)
    where
        V: ValueType;

    #[derive(Clone)]
    pub(crate) struct RedisCacheStorage {
        inner: Arc<Mutex<Connection>>,
    }

    fn get_type_of<T>(_: &T) -> &'static str {
        std::any::type_name::<T>()
    }

    impl<K> fmt::Display for RedisKey<K>
    where
        K: KeyType,
    {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl<K> ToRedisArgs for RedisKey<K>
    where
        K: KeyType,
    {
        fn write_redis_args<W>(&self, out: &mut W)
        where
            W: ?Sized + RedisWrite,
        {
            out.write_arg_fmt(self);
        }
    }

    impl<V> fmt::Display for RedisValue<V>
    where
        V: ValueType,
    {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}|{:?}", get_type_of(&self.0), self.0)
        }
    }

    impl<V> ToRedisArgs for RedisValue<V>
    where
        V: ValueType,
    {
        fn write_redis_args<W>(&self, out: &mut W)
        where
            W: ?Sized + RedisWrite,
        {
            let v = serde_json::to_vec(&self.0).unwrap();
            out.write_arg(&v);
        }
    }

    impl<V> FromRedisValue for RedisValue<V>
    where
        V: ValueType,
    {
        fn from_redis_value(v: &redis::Value) -> RedisResult<Self> {
            match v {
                redis::Value::Bulk(bulk_data) => {
                    for entry in bulk_data {
                        tracing::trace!("entry: {:?}", entry);
                        // entry.parse::<V>().unwrap()
                    }
                    Err(redis::RedisError::from((
                        redis::ErrorKind::TypeError,
                        "the data is the wrong type",
                    )))
                }
                redis::Value::Data(v) => serde_json::from_slice(v).map(RedisValue).map_err(|e| {
                    redis::RedisError::from((
                        redis::ErrorKind::TypeError,
                        "can't deserialize from JSON",
                        e.to_string(),
                    ))
                }),
                res => Err(redis::RedisError::from((
                    redis::ErrorKind::TypeError,
                    "the data is the wrong type",
                    format!("{:?}", res),
                ))),
            }
        }
    }

    impl RedisCacheStorage {
        pub(crate) async fn new(urls: Vec<String>) -> Self {
            let client = Client::open(urls).expect("opening ClusterClient");
            let connection = client.get_connection().await.expect("got redis connection");

            tracing::trace!("redis connection established");
            Self {
                inner: Arc::new(Mutex::new(connection)),
            }
        }

        pub(crate) async fn get<K: KeyType, V: ValueType>(
            &self,
            key: RedisKey<K>,
        ) -> Option<RedisValue<V>> {
            tracing::trace!("getting from redis: {:?}", key);
            let mut guard = self.inner.lock().await;
            guard.get(key).await.ok()
        }

        pub(crate) async fn insert<K: KeyType, V: ValueType>(
            &self,
            key: RedisKey<K>,
            value: RedisValue<V>,
        ) {
            tracing::trace!("inserting into redis: {:?}, {:?}", key, value);
            let mut guard = self.inner.lock().await;
            let r = guard
                .set::<RedisKey<K>, RedisValue<V>, redis::Value>(key, value)
                .await;
            tracing::trace!("insert result {:?}", r);
        }
    }
}
