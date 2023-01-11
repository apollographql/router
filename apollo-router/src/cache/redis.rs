// This entire file is license key functionality

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

enum RedisConnection {
    Single(redis::aio::Connection),
    Cluster(Connection),
}

#[derive(Clone)]
pub(crate) struct RedisCacheStorage {
    inner: Arc<Mutex<RedisConnection>>,
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
        let v = serde_json::to_vec(&self.0)
            .expect("JSON serialization should not fail for redis values");
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
    pub(crate) async fn new(mut urls: Vec<String>) -> Result<Self, redis::RedisError> {
        let connection = if urls.len() == 1 {
            let client = redis::Client::open(urls.pop().expect("urls contains only one url; qed"))?;
            let connection = client.get_async_connection().await?;
            RedisConnection::Single(connection)
        } else {
            let client = Client::open(urls)?;
            let connection = client.get_connection().await?;
            RedisConnection::Cluster(connection)
        };

        tracing::trace!("redis connection established");
        Ok(Self {
            inner: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) async fn get<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
    ) -> Option<RedisValue<V>> {
        tracing::trace!("getting from redis: {:?}", key);
        let mut guard = self.inner.lock().await;
        match &mut *guard {
            RedisConnection::Single(conn) => conn.get(key).await.ok(),
            RedisConnection::Cluster(conn) => conn.get(key).await.ok(),
        }
    }

    pub(crate) async fn insert<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
        value: RedisValue<V>,
    ) {
        tracing::trace!("inserting into redis: {:?}, {:?}", key, value);
        let mut guard = self.inner.lock().await;
        let r = match &mut *guard {
            RedisConnection::Single(conn) => {
                conn.set::<RedisKey<K>, RedisValue<V>, redis::Value>(key, value)
                    .await
            }
            RedisConnection::Cluster(conn) => {
                conn.set::<RedisKey<K>, RedisValue<V>, redis::Value>(key, value)
                    .await
            }
        };
        tracing::trace!("insert result {:?}", r);
    }
}
