use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

use lru::LruCache;
use redis::aio::Connection;
use redis::AsyncCommands;
use redis::Client;
use redis::FromRedisValue;
use redis::RedisResult;
use redis::RedisWrite;
use redis::ToRedisArgs;
use tokio::sync::Mutex;

pub(crate) trait KeyType: Clone + fmt::Debug + Hash + Eq + Send + Sync {}
pub(crate) trait ValueType: Clone + fmt::Debug + Send + Sync {}

// Blanket implementation which satisfies the compiler
impl<K> KeyType for K
where
    K: Clone + fmt::Debug + Hash + Eq + Send + Sync,
{
    // Nothing to implement, since K already supports the other traits.
    // It has the functions it needs already
}

// Blanket implementation which satisfies the compiler
impl<V> ValueType for V
where
    V: Clone + fmt::Debug + Send + Sync,
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
    redis: RedisCacheStorage,
}

impl<K, V> CacheStorage<K, V>
where
    K: KeyType,
    V: ValueType,
{
    pub(crate) async fn new(max_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
            redis: RedisCacheStorage::new().await,
        }
    }

    pub(crate) async fn get(&self, key: &K) -> Option<V> {
        match self.inner.lock().await.get(key) {
            Some(v) => Some(v.clone()),
            None => {
                let inner_key = RedisKey(key.clone());
                match self.redis.get(inner_key).await {
                    Some(v) => Some(v.0),
                    None => None,
                }
            }
        }
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        self.redis
            .insert(RedisKey(key.clone()), RedisValue(value.clone()))
            .await;
        self.inner.lock().await.put(key, value);
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct RedisKey<K>(K)
where
    K: KeyType;

#[derive(Clone)]
struct RedisValue<V>(V)
where
    V: ValueType;

#[derive(Clone)]
struct RedisCacheStorage {
    inner: Arc<Mutex<Connection>>,
}

impl<K> fmt::Display for RedisKey<K>
where
    K: KeyType,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
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
        write!(f, "{:?}", self.0)
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
        out.write_arg_fmt(self);
    }
}

impl<V> FromRedisValue for RedisValue<V>
where
    V: ValueType,
{
    fn from_redis_value(v: &redis::Value) -> RedisResult<Self> {
        tracing::info!("TRYING TO WORK WITH: {:?}", v);
        // let this: RedisValue<V> = RedisValue(V::from_str(v));
        // RedisResult
        // Ok(RedisValue(v.into()))
        todo!()
    }
}

impl RedisCacheStorage {
    async fn new() -> Self {
        let client = Client::open(
            "redis://:CzcBquHIjm@redis-redis-cluster-headless.redis.svc.cluster.local:6379",
        )
        .expect("opening ClusterClient");
        let connection = client
            .get_async_connection()
            .await
            .expect("got redis connection");
        /*
        let _: () = connection
            .set("test", "test_data")
            .await
            .expect("setting data");
        let rv: String = connection.get("test").await.expect("getting data");
        tracing::info!("rv: {:?}", rv);
        */

        Self {
            inner: Arc::new(Mutex::new(connection)),
        }
    }

    async fn get<K: KeyType, V: ValueType>(&self, key: RedisKey<K>) -> Option<RedisValue<V>> {
        let mut guard = self.inner.lock().await;
        guard.get(key).await.ok()
    }

    async fn insert<K: KeyType, V: ValueType>(&self, key: RedisKey<K>, value: RedisValue<V>) {
        let mut guard = self.inner.lock().await;
        guard
            .set::<RedisKey<K>, RedisValue<V>, RedisValue<V>>(key, value)
            .await
            .ok();
    }

    #[cfg(test)]
    async fn len(&self) -> usize {
        let mut guard = self.inner.lock().await;
        redis::cmd("DBSIZE")
            .query_async(&mut *guard)
            .await
            .expect("DBSIZE should work")
    }
}
