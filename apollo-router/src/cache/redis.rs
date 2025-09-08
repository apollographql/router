use std::collections::HashMap;
use std::fmt;
use std::iter::repeat_n;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use fred::clients::Client;
use fred::clients::Pipeline;
use fred::interfaces::EventInterface;
#[cfg(test)]
use fred::mocks::Mocks;
use fred::prelude::Client as RedisClient;
use fred::prelude::ClientLike;
use fred::prelude::Error as RedisError;
use fred::prelude::ErrorKind as RedisErrorKind;
use fred::prelude::HeartbeatInterface;
use fred::prelude::KeysInterface;
use fred::prelude::Pool as RedisPool;
use fred::prelude::TcpConfig;
use fred::prelude::TracingConfig;
use fred::types::Builder;
use fred::types::Expiration;
use fred::types::FromValue;
use fred::types::cluster::ClusterRouting;
use fred::types::config::Config as RedisConfig;
use fred::types::config::ReconnectPolicy;
use fred::types::config::ReplicaConfig;
use fred::types::config::TlsConfig;
use fred::types::config::TlsHostMapping;
use fred::types::config::UnresponsiveConfig;
use fred::types::scan::ScanResult;
use futures::Stream;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::AbortHandle;
use tower::BoxError;
use url::Url;

use super::KeyType;
use super::ValueType;
use super::metrics::RedisMetricsCollector;
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

/// Timeout applied to internal Redis operations, such as TCP connection initialization, TLS handshakes, AUTH or HELLO, cluster health checks, etc.
const DEFAULT_INTERNAL_REDIS_TIMEOUT: Duration = Duration::from_secs(5);
/// Interval on which we send PING commands to the Redis servers.
const REDIS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Record a Redis error as a metric, independent of having an active connection
fn record_redis_error(error: &RedisError, caller: &'static str) {
    // Don't track NotFound errors as they're expected for cache misses

    let error_type = match error.kind() {
        RedisErrorKind::Config => "config",
        RedisErrorKind::Auth => "auth",
        RedisErrorKind::Routing => "routing",
        RedisErrorKind::IO => "io",
        RedisErrorKind::InvalidCommand => "invalid_command",
        RedisErrorKind::InvalidArgument => "invalid_argument",
        RedisErrorKind::Url => "url",
        RedisErrorKind::Protocol => "protocol",
        RedisErrorKind::Tls => "tls",
        RedisErrorKind::Canceled => "canceled",
        RedisErrorKind::Unknown => "unknown",
        RedisErrorKind::Timeout => "timeout",
        RedisErrorKind::Cluster => "cluster",
        RedisErrorKind::Parse => "parse",
        RedisErrorKind::Sentinel => "sentinel",
        RedisErrorKind::Replica => "replica",
        RedisErrorKind::NotFound => "not_found",
        RedisErrorKind::Backpressure => "backpressure",
    };

    u64_counter_with_unit!(
        "apollo.router.cache.redis.errors",
        "Number of Redis errors by type",
        "{error}",
        1,
        kind = caller,
        error_type = error_type
    );

    if !error.is_not_found() && !error.is_canceled() {
        tracing::error!(
            error_type = error_type,
            caller = caller,
            error = ?error,
            "Redis error occurred"
        );
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RedisKey<K>(pub(crate) K)
where
    K: KeyType;

impl From<String> for RedisKey<String> {
    fn from(value: String) -> Self {
        RedisKey(value)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RedisValue<V>(pub(crate) V)
where
    V: ValueType;

/// `DropSafeRedisPool` is a wrapper for `fred::prelude::RedisPool` which closes the pool's Redis
/// connections when it is dropped.
//
// Dev notes:
// * the inner `RedisPool` must be wrapped in an `Arc` because closing the connections happens
//   in a spawned async task.
// * why not just implement this within `Drop` for `RedisCacheStorage`? Because `RedisCacheStorage`
//   is cloned frequently throughout the router, and we don't want to close the connections
//   when each clone is dropped, only when the last instance is dropped.
struct DropSafeRedisPool {
    pool: Arc<RedisPool>,
    heartbeat_abort_handle: AbortHandle,
    // Metrics collector handles its own abort and gauges
    _metrics_collector: RedisMetricsCollector,
}

impl Deref for DropSafeRedisPool {
    type Target = RedisPool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

impl Drop for DropSafeRedisPool {
    fn drop(&mut self) {
        let inner = self.pool.clone();
        tokio::spawn(async move {
            let result = inner.quit().await;
            if let Err(err) = result {
                tracing::warn!("Caught error while closing unused Redis connections: {err:?}");
            }
        });
        self.heartbeat_abort_handle.abort();
        // Metrics collector will be dropped automatically and its Drop impl will abort the task
    }
}

#[derive(Clone)]
pub(crate) struct RedisCacheStorage {
    inner: Arc<DropSafeRedisPool>,
    namespace: Option<Arc<String>>,
    pub(crate) ttl: Option<Duration>,
    is_cluster: bool,
    reset_ttl: bool,
    caller: &'static str,
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

impl<K> From<RedisKey<K>> for fred::types::Key
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

impl<V> FromValue for RedisValue<V>
where
    V: ValueType,
{
    fn from_value(value: fred::types::Value) -> Result<Self, RedisError> {
        match value {
            fred::types::Value::Bytes(data) => {
                serde_json::from_slice(&data).map(RedisValue).map_err(|e| {
                    RedisError::new(
                        RedisErrorKind::Parse,
                        format!("can't deserialize from JSON: {e}"),
                    )
                })
            }
            fred::types::Value::String(s) => {
                serde_json::from_str(&s).map(RedisValue).map_err(|e| {
                    RedisError::new(
                        RedisErrorKind::Parse,
                        format!("can't deserialize from JSON: {e}"),
                    )
                })
            }
            fred::types::Value::Null => Err(RedisError::new(RedisErrorKind::NotFound, "not found")),
            _res => Err(RedisError::new(
                RedisErrorKind::Parse,
                "the data is the wrong type",
            )),
        }
    }
}

impl<V> TryInto<fred::types::Value> for RedisValue<V>
where
    V: ValueType,
{
    type Error = RedisError;

    fn try_into(self) -> Result<fred::types::Value, Self::Error> {
        let v = serde_json::to_vec(&self.0).map_err(|e| {
            tracing::error!("couldn't serialize value to redis {}. This is a bug in the router, please file an issue: https://github.com/apollographql/router/issues/new", e);
            RedisError::new(
                RedisErrorKind::Parse,
                format!("couldn't serialize value to redis {e}"),
            )
        })?;

        Ok(fred::types::Value::Bytes(v.into()))
    }
}

impl RedisCacheStorage {
    pub(crate) async fn new(config: RedisCache, caller: &'static str) -> Result<Self, BoxError> {
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
                connector: fred::types::config::TlsConnector::Rustls(connector),
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
            caller,
            config.metrics_interval,
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
            "test",
            Duration::from_millis(100),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_client(
        client_config: RedisConfig,
        timeout: Duration,
        pool_size: usize,
        namespace: Option<String>,
        ttl: Option<Duration>,
        reset_ttl: bool,
        is_cluster: bool,
        caller: &'static str,
        metrics_interval: Duration,
    ) -> Result<Self, BoxError> {
        let pooled_client = Builder::from_config(client_config)
            .with_connection_config(|config| {
                config.internal_command_timeout = DEFAULT_INTERNAL_REDIS_TIMEOUT;
                config.reconnect_on_auth_error = true;
                config.max_command_buffer_len = 10_000;
                config.tcp = TcpConfig {
                    #[cfg(target_os = "linux")]
                    user_timeout: Some(timeout),
                    ..Default::default()
                };
                config.unresponsive = UnresponsiveConfig {
                    max_timeout: Some(DEFAULT_INTERNAL_REDIS_TIMEOUT),
                    interval: Duration::from_secs(3),
                };
                config.replica = ReplicaConfig {
                    lazy_connections: false,
                    primary_fallback: true,
                    ..Default::default()
                };
            })
            .with_performance_config(|config| {
                config.default_command_timeout = timeout;
            })
            .with_config(|config| {
                config.tracing = TracingConfig::new(true);
            })
            .set_policy(ReconnectPolicy::new_exponential(0, 1, 2000, 5))
            .build_pool(pool_size)?;

        for client in pooled_client.clients() {
            // spawn tasks that listen for connection close or reconnect events
            let mut error_rx = client.error_rx();
            let mut reconnect_rx = client.reconnect_rx();

            i64_up_down_counter_with_unit!(
                "apollo.router.cache.redis.connections",
                "Number of Redis connections",
                "{connection}",
                1,
                kind = caller
            );

            tokio::spawn(async move {
                loop {
                    match error_rx.recv().await {
                        Ok((error, Some(server))) => {
                            tracing::error!(
                                "Redis client disconnected from {server:?} with error: {error:?}",
                            )
                        }
                        Ok((error, None)) => {
                            tracing::error!("Redis client disconnected with error: {error:?}",)
                        }
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            tokio::spawn(async move {
                loop {
                    match reconnect_rx.recv().await {
                        Ok(server) => tracing::info!("Redis client connected to {server:?}"),
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    }
                }

                // NB: closing the Redis client connection will also close the error, pubsub, and
                // reconnection event streams, so the above while loop will only terminate when the
                // connection closes.
                i64_up_down_counter_with_unit!(
                    "apollo.router.cache.redis.connections",
                    "Number of Redis connections",
                    "{connection}",
                    -1,
                    kind = caller
                );
            });
        }

        let _handle = pooled_client.init().await.inspect_err(|e| {
            // Record connection failure as metrics even when initial setup fails
            record_redis_error(e, caller);
        })?;
        let heartbeat_clients = pooled_client.clone();
        let heartbeat_handle = tokio::spawn(async move {
            heartbeat_clients
                .enable_heartbeat(REDIS_HEARTBEAT_INTERVAL, false)
                .await
        });

        let pooled_client_arc = Arc::new(pooled_client);
        let metrics_collector =
            RedisMetricsCollector::new(pooled_client_arc.clone(), caller, metrics_interval);

        tracing::trace!("redis connection established");
        Ok(Self {
            inner: Arc::new(DropSafeRedisPool {
                pool: pooled_client_arc,
                heartbeat_abort_handle: heartbeat_handle.abort_handle(),
                _metrics_collector: metrics_collector,
            }),
            namespace: namespace.map(Arc::new),
            ttl,
            is_cluster,
            reset_ttl,
            caller,
        })
    }

    pub(crate) fn pipeline(&self) -> Pipeline<Client> {
        self.inner.next().pipeline()
    }

    pub(crate) fn client(&self) -> Client {
        self.inner.next().clone()
    }

    pub(crate) fn ttl(&self) -> Option<Duration> {
        self.ttl
    }

    /// Helper method to record Redis errors for metrics
    fn record_error(&self, error: &RedisError) {
        record_redis_error(error, self.caller);
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
                            "invalid Redis URL scheme, expected a scheme from {SUPPORTED_REDIS_SCHEMES:?}, got: {scheme}"
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

    pub(crate) fn make_key<K: KeyType>(&self, key: RedisKey<K>) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}:{key}"),
            None => key.to_string(),
        }
    }

    pub(crate) async fn get<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
    ) -> Result<RedisValue<V>, RedisError> {
        match self.ttl {
            Some(ttl) if self.reset_ttl => {
                let pipeline: Pipeline<RedisClient> = self.pipeline();
                let key = self.make_key(key);
                let _: () = pipeline
                    .get(&key)
                    .await
                    .inspect_err(|e| self.record_error(e))?;

                let _: () = pipeline
                    .expire(&key, ttl.as_secs() as i64, None)
                    .await
                    .inspect_err(|e| self.record_error(e))?;

                let (value, _exp_time): (RedisValue<V>, i64) =
                    pipeline.all().await.inspect_err(|e| self.record_error(e))?;
                Ok(value)
            }
            _ => {
                let full_key = self.make_key(key);
                self.inner
                    .get(full_key)
                    .await
                    .inspect_err(|e| self.record_error(e))
            }
        }
    }

    pub(crate) async fn get_multiple<K: KeyType, V: ValueType>(
        &self,
        keys: Vec<RedisKey<K>>,
    ) -> Vec<Option<RedisValue<V>>> {
        // NB: MGET is different from GET in that it returns Options rather than Results
        //  > For every key that does not hold a string value or does not exist, the special value
        //    nil is returned. Because of this, the operation never fails.
        //    - https://redis.io/docs/latest/commands/mget/
        tracing::trace!("getting multiple values from redis: {:?}", keys);

        // TODO: handle redis sentinel? I think that has replicas?
        if self.is_cluster {
            // TODO: shortcircuit for keys.len() == 1?
            // when using a cluster of redis nodes, the keys are hashed, and the hash number indicates which
            // node will store it. So first we have to group the keys by hash, because we cannot do a MGET
            // across multiple nodes (error: "ERR CROSSSLOT Keys in request don't hash to the same slot")
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

            let now = Instant::now();
            // let num_fetches = h.len();
            let mut tasks = Vec::new();
            for (_shard, (indexes, keys)) in h {
                // NB: use replica for fetch
                let client = self.inner.replicas();
                tasks.push(async move {
                    let result: Result<Vec<Option<RedisValue<V>>>, RedisError> =
                        client.mget(keys).await;
                    (indexes, result)
                });
            }
            let results = futures::future::join_all(tasks).await;
            f64_histogram_with_unit!(
                "apollo.router.cache.fetch.duration",
                "Duration of parallel clustered cache fetch",
                "s",
                now.elapsed().as_secs_f64(),
                uses_replicas = true,
                storage = "redis"
            );

            // then we have to assemble the results, by making sure that the values are in the same order as
            // the keys argument's order
            let now = Instant::now();
            let mut res = Vec::with_capacity(len);
            for (indexes, result) in results.into_iter() {
                let values: Vec<Option<RedisValue<V>>> = match result {
                    Ok(values) => values,
                    Err(err) => {
                        self.record_error(&err);
                        repeat_n(None, indexes.len()).collect()
                    }
                };
                for (index, value) in indexes.into_iter().zip(values.into_iter()) {
                    res.push((index, value));
                }
            }
            res.sort_by(|(i, _), (j, _)| i.cmp(j));
            let result = res.into_iter().map(|(_, v)| v).collect();

            f64_histogram_with_unit!(
                "apollo.router.cache.fetch_manipulation.duration",
                "Duration of fetch manipulation",
                "s",
                now.elapsed().as_secs_f64(),
                storage = "redis"
            );
            result
        } else if keys.len() == 1 {
            let key = keys.into_iter().next().unwrap();
            let res = self
                .inner
                .get::<RedisValue<V>, _>(self.make_key(key))
                .await
                .inspect_err(|e| self.record_error(e))
                .ok();
            vec![res]
        } else {
            let num_elements = keys.len();
            let result: Result<Vec<Option<RedisValue<V>>>, RedisError> = self
                .inner
                .mget(
                    keys.into_iter()
                        .map(|k| self.make_key(k))
                        .collect::<Vec<_>>(),
                )
                .await;

            match result {
                Ok(values) => values,
                Err(err) => {
                    self.record_error(&err);
                    repeat_n(None, num_elements).collect()
                }
            }
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
            .await
            .inspect_err(|e| self.record_error(e));
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
        }
        .inspect_err(|e| self.record_error(e));
        tracing::trace!("insert result {:?}", r);
    }

    /// Delete keys *without* adding the `namespace` prefix because `keys` is from
    /// `scan_with_namespaced_results` and already includes it.
    pub(crate) async fn delete_from_scan_result<I>(&self, keys: I) -> Vec<Result<u32, RedisError>>
    where
        I: IntoIterator<Item = fred::types::Key>,
    {
        let mut h: HashMap<u16, Vec<fred::types::Key>> = HashMap::new();
        for key in keys.into_iter() {
            let hash = ClusterRouting::hash_key(key.as_bytes());
            let entry = h.entry(hash).or_default();
            entry.push(key);
        }

        // then we query all the key groups at the same time
        let results: Vec<Result<u32, RedisError>> =
            futures::future::join_all(h.into_values().map(|keys| self.inner.del(keys))).await;

        for r in &results {
            if let Err(err) = r.as_ref() {
                self.record_error(err);
            }
        }

        results
    }

    /// The keys returned in `ScanResult` do include the prefix from `namespace` configuration.
    pub(crate) fn scan_with_namespaced_results(
        &self,
        pattern: String,
        count: Option<u32>,
    ) -> Pin<Box<dyn Stream<Item = Result<ScanResult, RedisError>> + Send>> {
        let pattern = self.make_key(RedisKey(pattern));
        if self.is_cluster {
            Box::pin(self.inner.next().scan_cluster(pattern, count, None))
        } else {
            Box::pin(self.inner.next().scan(pattern, count, None))
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn truncate_namespace(&self) {
        use fred::prelude::Key;
        use futures::StreamExt;

        if self.namespace.is_none() {
            return;
        }

        let pattern = self.make_key(RedisKey("*"));
        let client = self.client();
        let mut stream: Pin<Box<dyn Stream<Item = Result<Key, RedisError>>>> = if self.is_cluster {
            Box::pin(client.scan_cluster_buffered(pattern, None, None))
        } else {
            Box::pin(client.scan_buffered(pattern, None, None))
        };

        let mut keys = Vec::new();
        while let Some(res) = stream.next().await {
            if let Ok(key) = res {
                keys.push(key)
            }
        }

        self.delete_from_scan_result(keys).await;
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

        let as_value: Result<fred::types::Value, _> = invalid_json_payload.try_into();

        assert!(as_value.is_err());
    }

    #[test]
    fn it_preprocesses_redis_schemas_correctly() {
        // Base Format
        for scheme in ["redis", "rediss"] {
            let url_s = format!("{scheme}://username:password@host:6666/database");
            let url = Url::parse(&url_s).expect("it's a valid url");
            let urls = vec![url.clone(), url];
            assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        }
        // Cluster Format
        for scheme in ["redis-cluster", "rediss-cluster"] {
            let url_s =
                format!("{scheme}://username:password@host:6666?node=host1:6667&node=host2:6668");
            let url = Url::parse(&url_s).expect("it's a valid url");
            let urls = vec![url.clone(), url];
            assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        }
        // Sentinel Format
        for scheme in ["redis-sentinel", "rediss-sentinel"] {
            let url_s = format!(
                "{scheme}://username:password@host:6666?node=host1:6667&node=host2:6668&sentinelServiceName=myservice&sentinelUserName=username2&sentinelPassword=password2"
            );
            let url = Url::parse(&url_s).expect("it's a valid url");
            let urls = vec![url.clone(), url];
            assert!(super::RedisCacheStorage::preprocess_urls(urls).is_ok());
        }
        // Make sure it fails on sample invalid schemes
        for scheme in ["wrong", "something"] {
            let url_s = format!("{scheme}://username:password@host:6666/database");
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
