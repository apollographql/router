use std::collections::HashMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use fred::interfaces::EventInterface;
#[cfg(test)]
use fred::mocks::Mocks;
use fred::prelude::ClientLike;
use fred::prelude::KeysInterface;
use fred::prelude::RedisClient;
use fred::prelude::RedisError;
use fred::prelude::RedisErrorKind;
use fred::prelude::RedisPool;
use fred::types::ClusterRouting;
use fred::types::Expiration;
use fred::types::FromRedis;
use fred::types::PerformanceConfig;
use fred::types::ReconnectPolicy;
use fred::types::RedisConfig;
use fred::types::ScanResult;
use fred::types::TlsConfig;
use fred::types::TlsHostMapping;
use futures::FutureExt;
use futures::Stream;
use tower::BoxError;
use url::Url;

use super::KeyType;
use super::ValueType;
use crate::configuration::RedisCache;
use crate::services::generate_tls_client_config;

const SUPPORTED_REDIS_SCHEMES: [&str; 6] = [
    "redis",
    "rediss",
    "redis-cluster",
    "rediss-cluster",
    "redis-sentinel",
    "rediss-sentinel",
];

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
    inner: Arc<RedisPool>,
    namespace: Option<Arc<String>>,
    pub(crate) ttl: Option<Duration>,
    is_cluster: bool,
    reset_ttl: bool,
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
            fred::types::RedisValue::Null => {
                Err(RedisError::new(RedisErrorKind::NotFound, "not found"))
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
    pub(crate) async fn new(config: RedisCache) -> Result<Self, BoxError> {
        let url = Self::preprocess_urls(config.urls)?;
        let mut client_config = RedisConfig::from_url(url.as_str())?;
        let is_cluster = url.scheme() == "redis-cluster" || url.scheme() == "rediss-cluster";

        if let Some(username) = config.username {
            client_config.username = Some(username);
        }

        if let Some(password) = config.password {
            client_config.password = Some(password);
        }

        if let Some(tls) = config.tls.as_ref() {
            let tls_cert_store = tls.create_certificate_store().transpose()?;
            let client_cert_config = tls.client_authentication.as_ref();
            let tls_client_config = generate_tls_client_config(
                tls_cert_store,
                client_cert_config.map(|arc| arc.as_ref()),
            )?;
            let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_client_config));

            client_config.tls = Some(TlsConfig {
                connector: fred::types::TlsConnector::Rustls(connector),
                hostnames: TlsHostMapping::None,
            });
        }

        Self::create_client(
            client_config,
            config.timeout.unwrap_or(Duration::from_millis(500)),
            config.pool_size as usize,
            config.namespace,
            config.ttl,
            config.reset_ttl,
            is_cluster,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn from_mocks(mocks: Arc<dyn Mocks>) -> Result<Self, BoxError> {
        let client_config = RedisConfig {
            mocks: Some(mocks),
            ..Default::default()
        };

        Self::create_client(
            client_config,
            Duration::from_millis(2),
            1,
            None,
            None,
            false,
            false,
        )
        .await
    }

    async fn create_client(
        client_config: RedisConfig,
        timeout: Duration,
        pool_size: usize,
        namespace: Option<String>,
        ttl: Option<Duration>,
        reset_ttl: bool,
        is_cluster: bool,
    ) -> Result<Self, BoxError> {
        let pooled_client = RedisPool::new(
            client_config,
            Some(PerformanceConfig {
                default_command_timeout: timeout,
                ..Default::default()
            }),
            None,
            Some(ReconnectPolicy::new_exponential(0, 1, 2000, 5)),
            pool_size,
        )?;
        let _handle = pooled_client.connect();

        for client in pooled_client.clients() {
            // spawn tasks that listen for connection close or reconnect events
            let mut error_rx = client.error_rx();
            let mut reconnect_rx = client.reconnect_rx();

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
        }

        // a TLS connection to a TCP Redis could hang, so we add a timeout
        tokio::time::timeout(Duration::from_secs(5), pooled_client.wait_for_connect())
            .await
            .map_err(|_| {
                RedisError::new(RedisErrorKind::Timeout, "timeout connecting to Redis")
            })??;

        tracing::trace!("redis connection established");
        Ok(Self {
            inner: Arc::new(pooled_client),
            namespace: namespace.map(Arc::new),
            ttl,
            is_cluster,
            reset_ttl,
        })
    }

    pub(crate) fn ttl(&self) -> Option<Duration> {
        self.ttl
    }

    fn preprocess_urls(urls: Vec<Url>) -> Result<Url, RedisError> {
        let url_len = urls.len();
        let mut urls_iter = urls.into_iter();
        let first = urls_iter.next();
        match first {
            None => Err(RedisError::new(
                RedisErrorKind::Config,
                "empty Redis URL list",
            )),
            Some(first) => {
                if url_len == 1 {
                    return Ok(first.clone());
                }

                let username = first.username();
                let password = first.password();

                let scheme = first.scheme();

                let mut result = first.clone();

                if SUPPORTED_REDIS_SCHEMES.contains(&scheme) {
                    let _ = result.set_scheme(scheme);
                } else {
                    return Err(RedisError::new(
                        RedisErrorKind::Config,
                        format!(
                            "invalid Redis URL scheme, expected a scheme from {SUPPORTED_REDIS_SCHEMES:?}, got: {}",
                            scheme
                        ),
                    ));
                }

                for mut url in urls_iter {
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

                    // Backwords compatibility with old redis client
                    // If our url has a scheme of redis or rediss, convert it to be cluster form
                    // and if our result is of matching scheme, convert that to be cluster form.
                    if url.scheme() == "redis" {
                        let _ = url.set_scheme("redis-cluster");
                        if result.scheme() == "redis" {
                            let _ = result.set_scheme("redis-cluster");
                        }
                    }
                    if url.scheme() == "rediss" {
                        let _ = url.set_scheme("rediss-cluster");
                        if result.scheme() == "rediss" {
                            let _ = result.set_scheme("rediss-cluster");
                        }
                    }
                    // Now check to make sure our schemes match
                    if url.scheme() != result.scheme() {
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

    #[allow(dead_code)]
    pub(crate) fn set_ttl(&mut self, ttl: Option<Duration>) {
        self.ttl = ttl;
    }

    fn make_key<K: KeyType>(&self, key: RedisKey<K>) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}:{key}"),
            None => key.to_string(),
        }
    }

    pub(crate) async fn get<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
    ) -> Option<RedisValue<V>> {
        match self.ttl {
            Some(ttl) if self.reset_ttl => {
                let pipeline: fred::clients::Pipeline<RedisClient> = self.inner.next().pipeline();
                let key = self.make_key(key);
                let res = pipeline
                    .get::<fred::types::RedisValue, _>(&key)
                    .await
                    .map_err(|e| {
                        if !e.is_not_found() {
                            tracing::error!(error = %e, "redis get error");
                        }
                        e
                    })
                    .ok()?;
                if !res.is_queued() {
                    tracing::error!("could not queue GET command");
                    return None;
                }
                let res: fred::types::RedisValue = pipeline
                    .expire(&key, ttl.as_secs() as i64)
                    .await
                    .map_err(|e| {
                        if !e.is_not_found() {
                            tracing::error!(error = %e, "redis get error");
                        }
                        e
                    })
                    .ok()?;
                if !res.is_queued() {
                    tracing::error!("could not queue EXPIRE command");
                    return None;
                }

                let (first, _): (Option<RedisValue<V>>, bool) = pipeline
                    .all()
                    .await
                    .map_err(|e| {
                        if !e.is_not_found() {
                            tracing::error!(error = %e, "redis get error");
                        }
                        e
                    })
                    .ok()?;
                first
            }
            _ => self
                .inner
                .get::<RedisValue<V>, _>(self.make_key(key))
                .await
                .map_err(|e| {
                    if !e.is_not_found() {
                        tracing::error!(error = %e, "redis get error");
                    }
                    e
                })
                .ok(),
        }
    }

    pub(crate) async fn get_multiple<K: KeyType, V: ValueType>(
        &self,
        mut keys: Vec<RedisKey<K>>,
    ) -> Option<Vec<Option<RedisValue<V>>>> {
        tracing::trace!("getting multiple values from redis: {:?}", keys);

        if keys.len() == 1 {
            let res = self
                .inner
                .get::<RedisValue<V>, _>(self.make_key(keys.remove(0)))
                .await
                .map_err(|e| {
                    if !e.is_not_found() {
                        tracing::error!("get error: {}", e);
                    }
                    e
                })
                .ok();

            Some(vec![res])
        } else if self.is_cluster {
            // when using a cluster of redis nodes, the keys are hashed, and the hash number indicates which
            // node will store it. So first we have to group the keys by hash, because we cannot do a MGET
            // across multipe nodes (error: "ERR CROSSSLOT Keys in request don't hash to the same slot")
            let len = keys.len();
            let mut h: HashMap<u16, (Vec<usize>, Vec<String>)> = HashMap::new();
            for (index, key) in keys.into_iter().enumerate() {
                let key = self.make_key(key);
                let hash = ClusterRouting::hash_key(key.as_bytes());
                let entry = h.entry(hash).or_default();
                entry.0.push(index);
                entry.1.push(key);
            }

            // then we query all the key groups at the same time
            let results = futures::future::join_all(h.into_iter().map(|(_, (indexes, keys))| {
                self.inner
                    .mget(keys)
                    .map(|values: Result<Vec<Option<RedisValue<V>>>, RedisError>| (indexes, values))
            }))
            .await;

            // then we have to assemble the results, by making sure that the values are in the same order as
            // the keys argument's order
            let mut res = Vec::with_capacity(len);
            for (indexes, result) in results.into_iter() {
                match result {
                    Err(e) => {
                        tracing::error!("mget error: {}", e);
                        return None;
                    }
                    Ok(values) => {
                        for (index, value) in indexes.into_iter().zip(values.into_iter()) {
                            res.push((index, value));
                        }
                    }
                }
            }
            res.sort_by(|(i, _), (j, _)| i.cmp(j));
            Some(res.into_iter().map(|(_, v)| v).collect())
        } else {
            self.inner
                .mget(
                    keys.into_iter()
                        .map(|k| self.make_key(k))
                        .collect::<Vec<_>>(),
                )
                .await
                .map_err(|e| {
                    if !e.is_not_found() {
                        tracing::error!("mget error: {}", e);
                    }

                    e
                })
                .ok()
        }
    }

    pub(crate) async fn insert<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
        value: RedisValue<V>,
        ttl: Option<Duration>,
    ) {
        let key = self.make_key(key);
        tracing::trace!("inserting into redis: {:?}, {:?}", key, value);
        let expiration = ttl
            .as_ref()
            .or(self.ttl.as_ref())
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
        ttl: Option<Duration>,
    ) {
        tracing::trace!("inserting into redis: {:#?}", data);

        let r = match ttl.as_ref().or(self.ttl.as_ref()) {
            None => self.inner.mset(data.to_owned()).await,
            Some(ttl) => {
                let expiration = Some(Expiration::EX(ttl.as_secs() as i64));
                let pipeline = self.inner.next().pipeline();

                for (key, value) in data {
                    let _ = pipeline
                        .set::<(), _, _>(
                            self.make_key(key.clone()),
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

    pub(crate) async fn delete<K: KeyType>(&self, keys: Vec<RedisKey<K>>) -> Option<u32> {
        let mut h: HashMap<u16, Vec<String>> = HashMap::new();
        for key in keys.into_iter() {
            let key = self.make_key(key);
            let hash = ClusterRouting::hash_key(key.as_bytes());
            let entry = h.entry(hash).or_default();
            entry.push(key);
        }

        // then we query all the key groups at the same time
        let results: Vec<Result<u32, RedisError>> =
            futures::future::join_all(h.into_values().map(|keys| self.inner.del(keys))).await;
        let mut total = 0u32;

        for res in results {
            match res {
                Ok(res) => total += res,
                Err(e) => {
                    tracing::error!(error = %e, "redis del error");
                }
            }
        }

        Some(total)
    }

    pub(crate) fn scan(
        &self,
        pattern: String,
        count: Option<u32>,
    ) -> Pin<Box<dyn Stream<Item = Result<ScanResult, RedisError>> + Send>> {
        if self.is_cluster {
            Box::pin(self.inner.next().scan_cluster(pattern, count, None))
        } else {
            Box::pin(self.inner.next().scan(pattern, count, None))
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::SystemTime;

    use url::Url;

    use crate::cache::storage::ValueType;

    #[test]
    fn ensure_invalid_payload_serialization_doesnt_fail() {
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        struct Stuff {
            time: SystemTime,
        }
        impl ValueType for Stuff {
            fn estimated_size(&self) -> Option<usize> {
                None
            }
        }

        let invalid_json_payload = super::RedisValue(Stuff {
            // this systemtime is invalid, serialization will fail
            time: std::time::UNIX_EPOCH - std::time::Duration::new(1, 0),
        });

        let as_value: Result<fred::types::RedisValue, _> = invalid_json_payload.try_into();

        assert!(as_value.is_err());
    }

    #[test]
    fn it_preprocesses_redis_schemas_correctly() {
        // Base Format
        for scheme in ["redis", "rediss"] {
            let url_s = format!("{}://username:password@host:6666/database", scheme);
            let url = Url::parse(&url_s).expect("it's a valid url");
            let urls = vec![url.clone(), url];
            assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        }
        // Cluster Format
        for scheme in ["redis-cluster", "rediss-cluster"] {
            let url_s = format!(
                "{}://username:password@host:6666?node=host1:6667&node=host2:6668",
                scheme
            );
            let url = Url::parse(&url_s).expect("it's a valid url");
            let urls = vec![url.clone(), url];
            assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        }
        // Sentinel Format
        for scheme in ["redis-sentinel", "rediss-sentinel"] {
            let url_s = format!(
                "{}://username:password@host:6666?node=host1:6667&node=host2:6668&sentinelServiceName=myservice&sentinelUserName=username2&sentinelPassword=password2",
                scheme
            );
            let url = Url::parse(&url_s).expect("it's a valid url");
            let urls = vec![url.clone(), url];
            assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        }
        // Make sure it fails on sample invalid schemes
        for scheme in ["wrong", "something"] {
            let url_s = format!("{}://username:password@host:6666/database", scheme);
            let url = Url::parse(&url_s).expect("it's a valid url");
            let urls = vec![url.clone(), url];
            assert!(super::RedisCacheStorage::preprocess_urls(urls).is_err());
        }
    }

    // This isn't an exhaustive list of combinations, but some of the more common likely mistakes
    // that we should catch.
    #[test]
    fn it_preprocesses_redis_schemas_correctly_backwards_compatibility() {
        // Two redis schemes
        let url_s = "redis://username:password@host:6666/database";
        let url = Url::parse(url_s).expect("it's a valid url");
        let url_s1 = "redis://username:password@host:6666/database";
        let url_1 = Url::parse(url_s1).expect("it's a valid url");
        let urls = vec![url, url_1];
        assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        // redis-cluster, redis
        let url_s = "redis-cluster://username:password@host:6666/database";
        let url = Url::parse(url_s).expect("it's a valid url");
        let url_s1 = "redis://username:password@host:6666/database";
        let url_1 = Url::parse(url_s1).expect("it's a valid url");
        let urls = vec![url, url_1];
        assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        // redis, redis-cluster
        let url_s = "redis://username:password@host:6666/database";
        let url = Url::parse(url_s).expect("it's a valid url");
        let url_s1 = "redis-cluster://username:password@host:6666/database";
        let url_1 = Url::parse(url_s1).expect("it's a valid url");
        let urls = vec![url, url_1];
        assert!(super::RedisCacheStorage::preprocess_urls(urls).is_err());
        // redis-sentinel, redis
        let url_s = "redis-sentinel://username:password@host:6666/database";
        let url = Url::parse(url_s).expect("it's a valid url");
        let url_s1 = "redis://username:password@host:6666/database";
        let url_1 = Url::parse(url_s1).expect("it's a valid url");
        let urls = vec![url, url_1];
        assert!(super::RedisCacheStorage::preprocess_urls(urls).is_err());
        // redis, rediss
        let url_s = "redis://username:password@host:6666/database";
        let url = Url::parse(url_s).expect("it's a valid url");
        let url_s1 = "rediss://username:password@host:6666/database";
        let url_1 = Url::parse(url_s1).expect("it's a valid url");
        let urls = vec![url, url_1];
        assert!(super::RedisCacheStorage::preprocess_urls(urls).is_err());
        // redis, rediss-cluster
        let url_s = "redis://username:password@host:6666/database";
        let url = Url::parse(url_s).expect("it's a valid url");
        let url_s1 = "rediss-cluster://username:password@host:6666/database";
        let url_1 = Url::parse(url_s1).expect("it's a valid url");
        let urls = vec![url, url_1];
        assert!(super::RedisCacheStorage::preprocess_urls(urls).is_err());
    }
}
