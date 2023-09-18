use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use fred::interfaces::RedisResult;
use fred::prelude::ClientLike;
use fred::prelude::KeysInterface;
use fred::prelude::RedisClient;
use fred::prelude::RedisError;
use fred::prelude::RedisErrorKind;
use fred::types::Expiration;
use fred::types::FromRedis;
use fred::types::PerformanceConfig;
use fred::types::ReconnectPolicy;
use fred::types::RedisConfig;
use url::Url;

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

impl<K> From<RedisKey<K>> for fred::types::RedisKey
where
    K: KeyType,
{
    fn from(val: RedisKey<K>) -> Self {
        val.to_string().into()
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
                        format!("can't deserialize from JSON: {e}"),
                    )
                })
            }
            fred::types::RedisValue::String(s) => {
                serde_json::from_str(&s).map(RedisValue).map_err(|e| {
                    RedisError::new(
                        RedisErrorKind::Parse,
                        format!("can't deserialize from JSON: {e}"),
                    )
                })
            }
            _res => Err(RedisError::new(
                RedisErrorKind::Parse,
                "the data is the wrong type",
            )),
        }
    }
}

impl<V> TryInto<fred::types::RedisValue> for RedisValue<V>
where
    V: ValueType,
{
    type Error = RedisError;

    fn try_into(self) -> Result<fred::types::RedisValue, Self::Error> {
        let v = serde_json::to_vec(&self.0).map_err(|e| {
            tracing::error!("couldn't serialize value to redis {}. This is a bug in the router, please file an issue: https://github.com/apollographql/router/issues/new", e);
            RedisError::new(
                RedisErrorKind::Parse,
                format!("couldn't serialize value to redis {}", e),
            )
        })?;

        Ok(fred::types::RedisValue::Bytes(v.into()))
    }
}

impl RedisCacheStorage {
    pub(crate) async fn new(
        urls: Vec<Url>,
        ttl: Option<Duration>,
        timeout: Option<Duration>,
    ) -> Result<Self, RedisError> {
        let url = Self::preprocess_urls(urls)?;
        let config = RedisConfig::from_url(url.as_str())?;

        let client = RedisClient::new(
            config,
            Some(PerformanceConfig {
                default_command_timeout_ms: timeout.map(|t| t.as_millis() as u64).unwrap_or(2),
                ..Default::default()
            }),
            Some(ReconnectPolicy::new_exponential(0, 1, 2000, 5)),
        );
        let _handle = client.connect();

        // spawn tasks that listen for connection close or reconnect events
        let mut error_rx = client.on_error();
        let mut reconnect_rx = client.on_reconnect();

        tokio::spawn(async move {
            while let Ok(error) = error_rx.recv().await {
                tracing::error!("Client disconnected with error: {:?}", error);
            }
        });
        tokio::spawn(async move {
            while reconnect_rx.recv().await.is_ok() {
                tracing::info!("Redis client reconnected.");
            }
        });

        // a TLS connection to a TCP Redis could hang, so we add a timeout
        tokio::time::timeout(Duration::from_secs(5), client.wait_for_connect())
            .await
            .map_err(|_| {
                RedisError::new(RedisErrorKind::Timeout, "timeout connecting to Redis")
            })??;

        tracing::trace!("redis connection established");
        Ok(Self {
            inner: Arc::new(client),
            ttl,
        })
    }

    fn preprocess_urls(urls: Vec<Url>) -> Result<Url, RedisError> {
        match urls.get(0) {
            None => Err(RedisError::new(
                RedisErrorKind::Config,
                "empty Redis URL list",
            )),
            Some(first) => {
                if urls.len() == 1 {
                    return Ok(first.clone());
                }

                let username = first.username();
                let password = first.password();

                let scheme = first.scheme();

                let mut result = first.clone();

                match scheme {
                    "redis" => {
                        let _ = result.set_scheme("redis-cluster");
                    }
                    "rediss" => {
                        let _ = result.set_scheme("rediss-cluster");
                    }
                    other => {
                        return Err(RedisError::new(
                            RedisErrorKind::Config,
                            format!(
                                "invalid Redis URL scheme, expected 'redis' or 'rediss', got: {}",
                                other
                            ),
                        ))
                    }
                }

                for url in &urls[1..] {
                    if url.username() != username {
                        return Err(RedisError::new(
                            RedisErrorKind::Config,
                            "incompatible usernames between Redis URLs",
                        ));
                    }
                    if url.password() != password {
                        return Err(RedisError::new(
                            RedisErrorKind::Config,
                            "incompatible passwords between Redis URLs",
                        ));
                    }
                    if url.scheme() != scheme {
                        return Err(RedisError::new(
                            RedisErrorKind::Config,
                            "incompatible schemes between Redis URLs",
                        ));
                    }

                    let host = url.host_str().ok_or_else(|| {
                        RedisError::new(RedisErrorKind::Config, "missing host in Redis URL")
                    })?;

                    let port = url.port().ok_or_else(|| {
                        RedisError::new(RedisErrorKind::Config, "missing port in Redis URL")
                    })?;

                    result
                        .query_pairs_mut()
                        .append_pair("node", &format!("{host}:{port}"));
                }

                Ok(result)
            }
        }
    }

    pub(crate) fn set_ttl(&mut self, ttl: Option<Duration>) {
        self.ttl = ttl;
    }

    pub(crate) async fn get<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
    ) -> Option<RedisValue<V>> {
        tracing::trace!("getting from redis: {:?}", key);

        let result: RedisResult<String> = self.inner.get(key.to_string()).await;
        match result.as_ref().map(|s| s.as_str()) {
            // Fred returns nil rather than an error with not_found
            // See `RedisErrorKind::NotFound` for why this should work
            // To work around this we first read the value as a string and then deal with the value explicitly
            Ok("nil") => None,
            Ok(value) => serde_json::from_str(value)
                .map(RedisValue)
                .map_err(|e| {
                    tracing::error!("couldn't deserialize value from redis: {}", e);
                    e
                })
                .ok(),
            Err(e) => {
                if !e.is_not_found() {
                    tracing::error!("mget error: {}", e);
                }
                None
            }
        }
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
        let expiration = self
            .ttl
            .as_ref()
            .map(|ttl| Expiration::EX(ttl.as_secs() as i64));

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
    }
}

#[cfg(test)]
mod test {
    use std::time::SystemTime;

    #[test]
    fn ensure_invalid_payload_serialization_doesnt_fail() {
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        struct Stuff {
            time: SystemTime,
        }

        let invalid_json_payload = super::RedisValue(Stuff {
            // this systemtime is invalid, serialization will fail
            time: std::time::UNIX_EPOCH - std::time::Duration::new(1, 0),
        });

        let as_value: Result<fred::types::RedisValue, _> = invalid_json_payload.try_into();

        assert!(as_value.is_err());
    }
}
