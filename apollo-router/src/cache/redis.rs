// This entire file is license key functionality

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use fred::prelude::ClientLike;
use fred::prelude::KeysInterface;
use fred::prelude::RedisClient;
use fred::prelude::RedisError;
use fred::prelude::RedisErrorKind;
use fred::types::Expiration;
use fred::types::FromRedis;
use fred::types::ReconnectPolicy;
use fred::types::RedisConfig;

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
    inner: Arc<RedisClient>,
    ttl: Option<Duration>,
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

impl<K> Into<fred::types::RedisKey> for RedisKey<K>
where
    K: KeyType,
{
    fn into(self) -> fred::types::RedisKey {
        self.to_string().into()
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

impl<V> FromRedis for RedisValue<V>
where
    V: ValueType,
{
    fn from_value(value: fred::types::RedisValue) -> Result<Self, RedisError> {
        match value {
            fred::types::RedisValue::Bytes(data) => {
                serde_json::from_slice(&data).map(RedisValue).map_err(|e| {
                    RedisError::new(
                        RedisErrorKind::Parse,
                        format!("can't deserialize from JSON: {}", e.to_string()),
                    )
                })
            }
            fred::types::RedisValue::String(s) => serde_json::from_str(&s.to_string())
                .map(RedisValue)
                .map_err(|e| {
                    RedisError::new(
                        RedisErrorKind::Parse,
                        format!("can't deserialize from JSON: {}", e.to_string()),
                    )
                }),
            res => {
                println!("got redisvalue: {res:?}");
                Err(RedisError::new(
                    RedisErrorKind::Parse,
                    "the data is the wrong type",
                ))
            }
        }
    }
}

impl<V> TryInto<fred::types::RedisValue> for RedisValue<V>
where
    V: ValueType,
{
    type Error = RedisError;

    fn try_into(self) -> Result<fred::types::RedisValue, Self::Error> {
        let v = serde_json::to_vec(&self.0)
            .expect("JSON serialization should not fail for redis values");

        Ok(fred::types::RedisValue::Bytes(v.into()))
    }
}

impl RedisCacheStorage {
    pub(crate) async fn new(
        mut urls: Vec<String>,
        ttl: Option<Duration>,
    ) -> Result<Self, RedisError> {
        println!("{}: RedisCacheStorage::new", line!());
        /*let server_config = if urls.len() == 1 {
            let url = Url::parse(&urls.pop().expect("urls contains only one url; qed")).unwrap();
            println!("{}: RedisCacheStorage::new", line!());

            ServerConfig::new_centralized(url.host_str().unwrap(), url.port().unwrap())
        } else {
            let urls = urls
                .into_iter()
                .map(|u| Url::parse(&u).unwrap())
                .map(|u| (u.host_str().unwrap().to_string(), u.port().unwrap()))
                .collect::<Vec<_>>();

            println!("{}: RedisCacheStorage::new", line!());

            ServerConfig::new_clustered(urls)
        };

        println!(
            "{}: RedisCacheStorage::new server config = {server_config:?}",
            line!()
        );

        let tls = ClientConfig::builder()
            .with_safe_defaults()
            .with_native_roots()
            .with_no_client_auth();

        //FIXME: username and password
        let config = RedisConfig {
            server: server_config,
            tls: Some(tls.into()),
            ..Default::default()
        };*/

        let config = RedisConfig::from_url(urls.first().unwrap()).unwrap();
        println!("{}: RedisCacheStorage::new config: {config:?}", line!());

        let client = RedisClient::new(
            config,
            None, //perf policy
            Some(ReconnectPolicy::new_exponential(10, 1, 2000, 10)),
        );
        let _handle = client.connect();
        println!("{}: RedisCacheStorage::new will wait for connect", line!());

        // spawn tasks that listen for connection close or reconnect events
        let mut error_rx = client.on_error();
        let mut reconnect_rx = client.on_reconnect();

        tokio::spawn(async move {
            while let Ok(error) = error_rx.recv().await {
                println!("Client disconnected with error: {:?}", error);
            }
        });
        tokio::spawn(async move {
            while let Ok(_) = reconnect_rx.recv().await {
                println!("Client reconnected.");
            }
        });
        client.wait_for_connect().await.unwrap();

        println!("{}: RedisCacheStorage::new connected", line!());

        tracing::trace!("redis connection established");
        Ok(Self {
            inner: Arc::new(client),
            ttl,
        })
    }

    pub(crate) fn set_ttl(&mut self, ttl: Option<Duration>) {
        self.ttl = ttl;
    }

    pub(crate) async fn get<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
    ) -> Option<RedisValue<V>> {
        tracing::trace!("getting from redis: {:?}", key);

        self.inner
            .get(key.to_string())
            .await
            .map_err(|e| {
                tracing::error!("mget error: {}", e);
                e
            })
            .ok()
    }

    pub(crate) async fn get_multiple<K: KeyType, V: ValueType>(
        &self,
        keys: Vec<RedisKey<K>>,
    ) -> Option<Vec<Option<RedisValue<V>>>> {
        tracing::trace!("getting multiple values from redis: {:?}", keys);

        let res = if keys.len() == 1 {
            let res = self
                .inner
                .get::<RedisValue<V>, _>(keys.first().unwrap().to_string())
                .await
                .map_err(|e| {
                    tracing::error!("mget error: {}", e);
                    e
                })
                .ok();

            Some(vec![res])
        } else {
            self.inner
                .mget(
                    keys.clone()
                        .into_iter()
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>(),
                )
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
        let expiration = match self.ttl.as_ref() {
            Some(ttl) => Some(Expiration::EX(ttl.as_secs() as i64)),
            None => None,
        };
        let r = self
            .inner
            .set::<(), _, _>(key, value, expiration, None, false)
            .await;
        tracing::trace!("insert result {:?}", r);
    }

    pub(crate) async fn insert_multiple<K: KeyType, V: ValueType>(
        &self,
        data: &[(RedisKey<K>, RedisValue<V>)],
    ) {
        tracing::trace!("inserting into redis: {:#?}", data);

        let r = match self.ttl.as_ref() {
            None => self.inner.mset(data.to_owned()).await,
            Some(ttl) => {
                let expiration = Some(Expiration::EX(ttl.as_secs() as i64));
                let pipeline = self.inner.pipeline();

                for (key, value) in data {
                    let _ = pipeline
                        .set::<(), _, _>(
                            key.clone(),
                            value.clone(),
                            expiration.clone(),
                            None,
                            false,
                        )
                        .await;
                }

                pipeline.last().await
            }
        };
        tracing::trace!("insert result {:?}", r);

        //FIXME: expiration with pipeline

        /*if let Some(ttl) = self.ttl.as_ref() {
            let expiration: usize = ttl.as_secs().try_into().unwrap();
            let mut pipeline = redis::pipe();
            pipeline.atomic();

            for (key, value) in data {
                pipeline.set_ex(key, value, expiration);
            }

            let mut guard = self.inner.lock().await;

            let r = match &mut *guard {
                RedisConnection::Single(conn) => {
                    pipeline
                        .query_async::<redis::aio::Connection, redis::Value>(conn)
                        .await
                }
                RedisConnection::Cluster(conn) => {
                    pipeline.query_async::<Connection, redis::Value>(conn).await
                }
            };

            tracing::trace!("insert result {:?}", r);
        } else {
            let mut guard = self.inner.lock().await;

            let r = match &mut *guard {
                RedisConnection::Single(conn) => {
                    conn.set_multiple::<RedisKey<K>, RedisValue<V>, redis::Value>(data)
                        .await
                }
                RedisConnection::Cluster(conn) => {
                    conn.set_multiple::<RedisKey<K>, RedisValue<V>, redis::Value>(data)
                        .await
                }
            };

            tracing::trace!("insert result {:?}", r);
        }*/
        todo!()
    }
}
