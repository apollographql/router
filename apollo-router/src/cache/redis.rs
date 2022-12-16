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
    pub(crate) async fn new(urls: Vec<String>) -> Result<Self, redis::RedisError> {
        let client = Client::open(urls)?;
        let connection = client.get_connection().await?;

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
        let res = guard.get(&key).await.ok();

        res
    }

    pub(crate) async fn mget<K: KeyType, V: ValueType>(
        &self,
        keys: Vec<RedisKey<K>>,
    ) -> Option<Vec<Option<RedisValue<V>>>> {
        tracing::trace!("getting multiple values from redis: {:?}", keys);
        let mut guard = self.inner.lock().await;

        let res = if keys.len() == 1 {
            let res = guard
                .get(keys.first().unwrap())
                .await
                .map_err(|e| {
                    tracing::error!("mget error: {}", e);
                    e
                })
                .ok();
            Some(vec![res])
        } else {
            guard
                .get::<Vec<RedisKey<K>>, Vec<Option<RedisValue<V>>>>(keys.clone())
                .await
                .map_err(|e| {
                    tracing::error!("mget error: {}", e);
                    e
                })
                .ok()
        };
        tracing::trace!("result for '{:?}': {:?}", keys, res);

        res
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

    pub(crate) async fn insert_multiple<K: KeyType, V: ValueType>(
        &self,
        data: &[(RedisKey<K>, RedisValue<V>)],
    ) {
        tracing::trace!("inserting into redis: {:#?}", data);

        let mut guard = self.inner.lock().await;

        let r = guard
            .set_multiple::<RedisKey<K>, RedisValue<V>, redis::Value>(data)
            .await;
        tracing::trace!("insert result {:?}", r);
    }
}
