use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fred::clients::Client;
use fred::clients::Pipeline;
use fred::interfaces::EventInterface;
#[cfg(test)]
use fred::mocks::Mocks;
use fred::prelude::ClientLike;
use fred::prelude::Error as RedisError;
use fred::prelude::ErrorKind as RedisErrorKind;
use fred::prelude::HeartbeatInterface;
use fred::prelude::KeysInterface;
use fred::prelude::Options;
use fred::prelude::Pool as RedisPool;
use fred::prelude::TcpConfig;
use fred::types::Builder;
use fred::types::Expiration;
use fred::types::FromValue;
use fred::types::cluster::ClusterRouting;
use fred::types::config::ClusterDiscoveryPolicy;
use fred::types::config::Config as RedisConfig;
use fred::types::config::ReconnectPolicy;
use fred::types::config::TlsConfig;
use fred::types::config::TlsHostMapping;
use fred::types::config::UnresponsiveConfig;
use fred::types::scan::ScanResult;
use futures::Stream;
use futures::future::join_all;
use parking_lot::RwLock;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::AbortHandle;
use tower::BoxError;
use url::Url;

use super::KeyType;
use super::ValueType;
use super::metrics::RedisMetricsCollector;
use crate::configuration::RedisCache;
use crate::services::generate_tls_client_config;

pub(super) static ACTIVE_CLIENT_COUNT: AtomicU64 = AtomicU64::new(0);
const SUPPORTED_REDIS_SCHEMES: [&str; 6] = [
    "redis",
    "rediss",
    "redis-cluster",
    "rediss-cluster",
    "redis-sentinel",
    "rediss-sentinel",
];

/// Timeout applied to internal Redis operations, such as TCP connection initialization, TLS handshakes, AUTH or HELLO, cluster health checks, etc.
// NOTE: In practice we've found that 5s is too low, so we've set it to 10s. Do some sanity checking before tweaking it to a lower value
const DEFAULT_INTERNAL_REDIS_TIMEOUT: Duration = Duration::from_secs(10);
/// Interval on which we send PING commands to the Redis servers.
const REDIS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Record a Redis error as a metric and emits an error-level log for it, independent of having an active connection
fn record_redis_error(error: &RedisError, caller: &'static str, context: &'static str) {
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
        RedisErrorKind::NotFound => "not_found",
        RedisErrorKind::Backpressure => "backpressure",
        RedisErrorKind::Replica => "replica",
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
            context = context,
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
    // the caller is whatever feature is using redis (eg, response-cache is a caller); used in
    // metrics and so on
    caller: &'static str,
    heartbeat_abort_handle: AbortHandle,
    // Metrics collector handles its own abort and spawns a background task for gauge updates
    metrics_collector: RedisMetricsCollector,
}

impl DropSafeRedisPool {
    /// Signal that the meter provider is ready and metrics gauges can be created.
    fn activate(&self) {
        self.metrics_collector.activate();
    }
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
        let caller = self.caller;
        self.heartbeat_abort_handle.abort();
        tokio::spawn(async move {
            let _ = inner
                .quit()
                .await
                .inspect_err(|err| record_redis_error(err, caller, "shutdown"));
        });
        // Metrics collector will be dropped automatically and its Drop impl will abort the task
    }
}

// TODO: comment
#[derive(Clone)]
struct RedisClientConfig {
    client_config: RedisConfig,
    timeout: Duration,
    pool_size: usize,
    caller: &'static str,
    metrics_interval: Duration,
    required_to_start: bool,
}

// TODO: comment
#[derive(Clone)]
pub(crate) struct RedisCacheStorage {
    // TODO: notes on why parking_lot::RwLock, see reverted commit
    inner: Arc<RwLock<Option<DropSafeRedisPool>>>,
    namespace: Option<Arc<String>>,
    pub(crate) ttl: Option<Duration>,
    is_cluster: bool,
    reset_ttl: bool,
    // TODO: comment
    redis_client_config: RedisClientConfig,
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
    /// Create a new RedisCacheStorage without an inner client. To create an inner client, you must call `create_client`
    pub(crate) async fn new(config: RedisCache, caller: &'static str) -> Result<Self, BoxError> {
        let url = Self::preprocess_urls(config.urls)
            .inspect_err(|err| record_redis_error(err, caller, "startup"))?;
        let mut client_config = RedisConfig::from_url(url.as_str())
            .inspect_err(|err| record_redis_error(err, caller, "startup"))?;
        let is_cluster = client_config.server.is_clustered();

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

        let redis_client_config = RedisClientConfig {
            client_config,
            timeout: config.timeout,
            pool_size: config.pool_size as usize,
            caller,
            metrics_interval: config.metrics_interval,
            required_to_start: config.required_to_start,
        };

        Ok(Self {
            inner: Arc::new(RwLock::new(None)),
            redis_client_config,
            namespace: config.namespace.map(Arc::new),
            ttl: config.ttl,
            reset_ttl: config.reset_ttl,
            is_cluster,
        })
    }

    #[cfg(test)]
    pub(crate) async fn from_mocks(mocks: Arc<dyn Mocks>) -> Result<Self, BoxError> {
        let config = RedisCache {
            urls: vec![],
            username: None,
            password: None,
            timeout: Duration::from_millis(2),
            ttl: None,
            namespace: None,
            tls: None,
            required_to_start: false,
            reset_ttl: false,
            pool_size: 1,
            metrics_interval: Duration::from_millis(100),
        };

        Self::from_mocks_and_config(mocks, config, "test", false).await
    }

    #[cfg(test)]
    pub(crate) async fn from_mocks_and_config(
        mocks: Arc<dyn Mocks>,
        config: RedisCache,
        caller: &'static str,
        is_cluster: bool,
    ) -> Result<Self, BoxError> {
        let client_config = RedisConfig {
            mocks: Some(mocks),
            ..Default::default()
        };

        let redis_client_config = RedisClientConfig {
            client_config,
            timeout: config.timeout,
            pool_size: config.pool_size as usize,
            caller,
            metrics_interval: config.metrics_interval,
            required_to_start: config.required_to_start,
        };

        let storage = Self {
            inner: Arc::new(RwLock::new(None)),
            namespace: config.namespace.map(Arc::new),
            ttl: config.ttl,
            is_cluster,
            reset_ttl: config.reset_ttl,
            redis_client_config,
        };

        storage.create_client().await
    }

    /// Creates an inner client alongside some metadata (namespace, ttl, whether it's a clustered
    /// client, ttl reset) to create a fully fledged RedisCacheStorage
    // NOTE: this splits up the actual inner's creation from the RedisCacheStorage creation because
    // we need to have a way to recreate the inner client when it errors out but we don't need to
    // recreate all of RedisCacheStorage
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_client(self) -> Result<Self, BoxError> {
        let inner = self.create_inner_client().await?;
        *self.inner.write() = inner;
        Ok(self)
    }

    /// Creates the inner redis client for RedisCacheStorage
    async fn create_inner_client(&self) -> Result<Option<DropSafeRedisPool>, BoxError> {
        let pooled_client = Builder::from_config(self.redis_client_config.client_config.clone())
            .with_config(|client_config| {
                if self.is_cluster {
                    // use `ClusterDiscoveryPolicy::ConfigEndpoint` - explicit in case the default changes.
                    // this determines how the clients discover other cluster nodes
                    let _ = client_config
                        .server
                        .set_cluster_discovery_policy(ClusterDiscoveryPolicy::ConfigEndpoint)
                        .inspect_err(|err| {
                            record_redis_error(err, self.redis_client_config.caller, "startup")
                        });
                }
            })
            .with_connection_config(|config| {
                // NOTE: the default internal_command_timeout is 10s, so this line is just to make
                // it explicit that we're using that default (at the time of writing, this const is
                // set to 10s)
                config.internal_command_timeout = DEFAULT_INTERNAL_REDIS_TIMEOUT;
                config.max_command_buffer_len = 10_000;
                config.reconnect_on_auth_error = true;
                config.tcp = TcpConfig {
                    #[cfg(target_os = "linux")]
                    user_timeout: Some(self.redis_client_config.timeout),
                    ..Default::default()
                };
                config.unresponsive = UnresponsiveConfig {
                    max_timeout: Some(DEFAULT_INTERNAL_REDIS_TIMEOUT),
                    interval: Duration::from_secs(2),
                };

                // PR-8405: must not use lazy connections or else commands will queue rather than being sent
                // PR-8671: must only disable lazy connections in cluster mode. otherwise, fred will
                //  try to connect to unreachable replicas and fall over.
                //  https://github.com/aembke/fred.rs/blob/f222ad7bfba844dbdc57e93da61b0a5483858df9/src/router/replicas.rs#L34
                if self.is_cluster {
                    config.replica.lazy_connections = false;
                }
            })
            .with_performance_config(|config| {
                config.default_command_timeout = self.redis_client_config.timeout;
            })
            .set_policy(ReconnectPolicy::new_exponential(0, 1, 2000, 5))
            .build_pool(self.redis_client_config.pool_size)?;

        for client in pooled_client.clients() {
            setup_event_listeners(self.redis_client_config.caller, client);
            ACTIVE_CLIENT_COUNT.fetch_add(1, Ordering::Relaxed);
        }

        // NB: error is not recorded here as it will be observed by the task following `client.error_rx()`
        let client_handles = pooled_client.connect_pool();
        if self.redis_client_config.required_to_start {
            pooled_client.wait_for_connect().await?;
            tracing::trace!("redis connections established");
        }

        // We spawn a task for watching pool shutdown; shutdown can happen either when reconnection
        // attempts are exhausted or, if configured to never stop trying to reconnect, when quit()
        // is called by us (it's never called within fred)
        //
        // Don't mistake connections for clients. This is a pool of clients that handles
        // connections, and if a connection breaks, fred will internally (attempt to) recreate it
        tokio::spawn(async move {
            let results = join_all(client_handles).await;
            ACTIVE_CLIENT_COUNT.fetch_sub(results.len() as u64, Ordering::Relaxed);
        });

        let heartbeat_clients = pooled_client.clone();
        let heartbeat_handle = tokio::spawn(async move {
            heartbeat_clients
                .enable_heartbeat(REDIS_HEARTBEAT_INTERVAL, false)
                .await
        });

        let pooled_client_arc = Arc::new(pooled_client);
        let metrics_collector = RedisMetricsCollector::new(
            pooled_client_arc.clone(),
            self.redis_client_config.caller,
            self.redis_client_config.metrics_interval,
        );

        Ok(Some(DropSafeRedisPool {
            pool: pooled_client_arc,
            caller: self.redis_client_config.caller,
            heartbeat_abort_handle: heartbeat_handle.abort_handle(),
            metrics_collector,
        }))
    }

    pub(crate) fn ttl(&self) -> Option<Duration> {
        self.ttl
    }

    /// Signal that the meter provider is ready and metrics gauges can be created.
    ///
    /// This MUST be called after `Telemetry.activate()` to ensure gauges are
    /// registered with the correct meter provider.
    pub(crate) fn activate(&self) {
        if let Some(inner) = self.inner.read().as_ref() {
            inner.activate();
        }
    }

    /// Helper method to record Redis errors for metrics. Calls `record_redis_error` for both
    /// metrics recording but also emitting an error-level log for the error
    fn record_query_error(&self, error: &RedisError) {
        record_redis_error(error, self.redis_client_config.caller, "query");
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
                let scheme = first.scheme();
                if !SUPPORTED_REDIS_SCHEMES.contains(&scheme) {
                    return Err(RedisError::new(
                        RedisErrorKind::Config,
                        format!(
                            "invalid Redis URL scheme, expected a scheme from {SUPPORTED_REDIS_SCHEMES:?}, got: {scheme}"
                        ),
                    ));
                }

                if url_len == 1 {
                    return Ok(first.clone());
                }

                let username = first.username();
                let password = first.password();

                let mut result = first.clone();
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

                    // Backwards compatibility with old redis client
                    // If our url has a scheme of redis or rediss, convert it to be cluster form
                    // and if our result is of matching scheme, convert that to be cluster form.
                    for url_ref in [&mut url, &mut result] {
                        if url_ref.scheme() == "redis" {
                            let _ = url_ref.set_scheme("redis-cluster");
                        }
                        if url_ref.scheme() == "rediss" {
                            let _ = url_ref.set_scheme("rediss-cluster");
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

    // TODO: note on why RedisError for ease/compatibility/fewest changes for now
    pub(crate) async fn client(&self) -> Result<Client, RedisError> {
        // WARN: this looks funky but is important to leave alone: this creates a read guard, gets
        // the client out of the pool, and then drops the guard; this avoids a deadlock below when
        // we try to take a write guard out in scenarios where we're recreating the client
        let maybe_client = {
            let pool = self.inner.read();
            pool.as_ref().map(|pool| pool.next().clone())
        };

        match maybe_client {
            Some(client) => Ok(client),
            None => {
                let new_inner = match self.create_inner_client().await {
                    Ok(new_inner) => new_inner,
                    Err(e) => {
                        let error = RedisError::new(
                            RedisErrorKind::Unknown,
                            format!("Error attempting to recreate client: {e:?}"),
                        );
                        record_redis_error(&error, self.redis_client_config.caller, "client");
                        return Err(error);
                    }
                };

                *self.inner.write() = new_inner;

                // rather than get into either recursion or a loop, we just return an error and let
                // the current attempt to reach redis fail. We have a new client waiting for the
                // next attempt, so this should be a temporary failure
                let error = RedisError::new(
                    RedisErrorKind::Unknown,
                    "client being recreated after connection interrupt, retry command",
                );
                record_redis_error(&error, self.redis_client_config.caller, "client");
                Err(error)
            }
        }
    }

    pub(crate) async fn pipeline(&self) -> Result<Pipeline<Client>, RedisError> {
        // WARN: this looks funky but is important to leave alone: this creates a read guard, gets
        // the client out of the pool, and then drops the guard; this avoids a deadlock below when
        // we try to take a write guard out in scenarios where we're recreating the client
        let maybe_pipeline = {
            let pool = self.inner.read();
            pool.as_ref().map(|pool| pool.next().pipeline())
        };

        match maybe_pipeline {
            Some(pipeline) => Ok(pipeline),
            None => {
                let new_inner = match self.create_inner_client().await {
                    Ok(new_inner) => new_inner,
                    Err(e) => {
                        let error = RedisError::new(
                            RedisErrorKind::Unknown,
                            format!("Error attempting to recreate client: {e:?}"),
                        );
                        record_redis_error(&error, self.redis_client_config.caller, "client");
                        return Err(error);
                    }
                };

                *self.inner.write() = new_inner;

                // rather than get into either recursion or a loop, we just return an error and let
                // the current attempt to reach redis fail. We have a new client waiting for the
                // next attempt, so this should be a temporary failure
                let error = RedisError::new(
                    RedisErrorKind::Unknown,
                    "client being recreated after connection interrupt, retry command",
                );
                record_redis_error(&error, self.redis_client_config.caller, "client");
                Err(error)
            }
        }
    }

    fn expiration(&self, ttl: Option<Duration>) -> Option<Expiration> {
        let ttl = ttl.or(self.ttl)?;
        Some(Expiration::EX(ttl.as_secs() as i64))
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
        self.get_with_options(key, Options::default()).await
    }

    pub(crate) async fn get_with_options<K: KeyType, V: ValueType>(
        &self,
        key: RedisKey<K>,
        options: Options,
    ) -> Result<RedisValue<V>, RedisError> {
        let key = self.make_key(key);
        if self.reset_ttl
            && let Some(ttl) = self.ttl
        {
            let pipeline = self.pipeline().await?.with_options(&options);
            let _: () = pipeline
                .get(&key)
                .await
                .inspect_err(|e| self.record_query_error(e))?;
            let _: () = pipeline
                .expire(&key, ttl.as_secs() as i64, None)
                .await
                .inspect_err(|e| self.record_query_error(e))?;

            let (value, _timeout_set): (RedisValue<V>, bool) = pipeline
                .all()
                .await
                .inspect_err(|e| self.record_query_error(e))?;
            Ok(value)
        } else if self.is_cluster {
            let client = self.client().await?.replicas().with_options(&options);
            client
                .get(key)
                .await
                .inspect_err(|e| self.record_query_error(e))
        } else {
            let client = self.client().await?.with_options(&options);
            client
                .get(key)
                .await
                .inspect_err(|e| self.record_query_error(e))
        }
    }

    pub(crate) async fn get_multiple<K: KeyType, V: ValueType>(
        &self,
        keys: Vec<RedisKey<K>>,
    ) -> Result<Vec<Result<RedisValue<V>, RedisError>>, RedisError> {
        self.get_multiple_with_options(keys, Options::default())
            .await
    }

    /// `Result<Vec<Result<RedisValue<V>, RedisError>>, RedisError>` is a horrible return type but
    /// is needed to capture the multiple levels of errors that can occur.
    ///
    /// The outer `Result` covers total failures (ie the standalone node is down), while the inner
    /// `Result`s cover partial cluster failures and values not being found.
    pub(crate) async fn get_multiple_with_options<K: KeyType, V: ValueType>(
        &self,
        keys: Vec<RedisKey<K>>,
        options: Options,
    ) -> Result<Vec<Result<RedisValue<V>, RedisError>>, RedisError> {
        tracing::trace!("getting multiple values from redis: {:?}", keys);
        if self.is_cluster {
            // we cannot do an MGET across hash slots (error: "ERR CROSSSLOT Keys in request don't
            // hash to the same slot").
            // we either need to group the keys by hash slot, or just send a GET for each key; given
            // that there are 16384 slots and we're using multiplexing, there shouldn't be a
            // performance penalty by just sending a GET per key.
            let len = keys.len();

            // then we query all the key groups at the same time
            // use `client.replicas()` since we're in a cluster and can take advantage of read-replicas
            let client = self.client().await?.replicas().with_options(&options);
            let mut tasks = Vec::with_capacity(len);
            for (index, key) in keys.into_iter().enumerate() {
                let client = client.clone();
                tasks.push(async move {
                    let res_value: Result<RedisValue<V>, RedisError> =
                        client.get(self.make_key(key)).await;
                    (index, res_value)
                })
            }

            let mut results_with_indexes = join_all(tasks).await;
            results_with_indexes.sort_unstable_by_key(|(index, _)| *index);
            Ok(results_with_indexes
                .into_iter()
                .map(|(_, value)| value.inspect_err(|e| self.record_query_error(e)))
                .collect())
        } else {
            let keys = keys
                .into_iter()
                .map(|k| self.make_key(k))
                .collect::<Vec<_>>();
            let values: Vec<Option<RedisValue<V>>> = self
                .client()
                .await?
                .with_options(&options)
                .mget(keys)
                .await
                .inspect_err(|e| self.record_query_error(e))?;
            Ok(values
                .into_iter()
                .map(|v| v.ok_or(RedisError::new(RedisErrorKind::NotFound, "")))
                .collect())
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

        // NOTE: we need a writer, so don't use replicas() here
        match self.client().await {
            Ok(client) => {
                let result: Result<(), _> = client
                    .set(key, value, self.expiration(ttl), None, false)
                    .await;

                tracing::trace!("insert result {:?}", result);

                if let Err(err) = result {
                    self.record_query_error(&err);
                }
            }
            Err(err) => {
                self.record_query_error(&err);
            }
        }
    }

    /// Inserts multiple records. Returns Ok(()) on success, emitting traces for successful
    /// inserts, and otherwise an error and error traces and error-level log
    pub(crate) async fn insert_multiple<K: KeyType, V: ValueType>(
        &self,
        data: &[(RedisKey<K>, RedisValue<V>)],
        ttl: Option<Duration>,
    ) -> Result<(), RedisError> {
        tracing::trace!("inserting into redis: {:#?}", data);
        let expiration = self.expiration(ttl);

        // NB: if we were using MSET here, we'd need to split the keys by hash slot. however, fred
        // seems to split the pipeline by hash slot in the background.
        let pipeline = self.pipeline().await?;
        for (key, value) in data {
            let key = self.make_key(key.clone());
            let _: Result<(), _> = pipeline
                .set(key, value.clone(), expiration.clone(), None, false)
                .await;
        }

        let result: Result<Vec<()>, _> = pipeline.all().await;
        match result {
            Ok(values) => tracing::trace!("successfully inserted {} values", values.len()),
            Err(err) => {
                tracing::trace!("caught error during insert: {err:?}");
                self.record_query_error(&err);
            }
        }

        Ok(())
    }

    /// Delete keys *without* adding the `namespace` prefix because `keys` is from
    /// `scan_with_namespaced_results` and already includes it.
    pub(crate) async fn delete_from_scan_result<I>(&self, keys: I) -> Result<u32, RedisError>
    where
        I: Iterator<Item = fred::types::Key>,
    {
        self.delete_from_scan_result_with_options(keys, Options::default())
            .await
    }

    /// Delete keys *without* adding the `namespace` prefix because `keys` is from
    /// `scan_with_namespaced_results` and already includes it.
    pub(crate) async fn delete_from_scan_result_with_options<I>(
        &self,
        keys: I,
        options: Options,
    ) -> Result<u32, RedisError>
    where
        I: Iterator<Item = fred::types::Key>,
    {
        let mut h: HashMap<u16, Vec<fred::types::Key>> = HashMap::new();
        for key in keys.into_iter() {
            let hash = ClusterRouting::hash_key(key.as_bytes());
            let entry = h.entry(hash).or_default();
            entry.push(key);
        }

        // then we execute against all the key groups at the same time
        let results: Vec<Result<u32, RedisError>> = join_all(h.into_values().map(|keys| async {
            let client = self.client().await?.with_options(&options);
            client.del(keys).await
        }))
        .await;

        let mut total = 0;
        for result in results {
            let count = result.inspect_err(|e| self.record_query_error(e))?;
            total += count;
        }

        Ok(total)
    }

    /// The keys returned in `ScanResult` do include the prefix from `namespace` configuration.
    pub(crate) async fn scan_with_namespaced_results(
        &self,
        pattern: String,
        count: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ScanResult, RedisError>> + Send>>, RedisError>
    {
        let pattern = self.make_key(RedisKey(pattern));
        if self.is_cluster {
            // NOTE: scans might be better send to only the read replicas, but the read-only client
            // doesn't have a scan_cluster(), just a paginated version called scan_page()
            Ok(Box::pin(
                self.client().await?.scan_cluster(pattern, count, None),
            ))
        } else {
            Ok(Box::pin(self.client().await?.scan(pattern, count, None)))
        }
    }
}

/// TODO: add docs
fn setup_event_listeners(caller: &'static str, client: &Client) {
    let mut error_rx = client.error_rx();
    let mut reconnect_rx = client.reconnect_rx();
    let mut unresponsive_rx = client.unresponsive_rx();

    // listen for error events
    tokio::spawn(async move {
        loop {
            match error_rx.recv().await {
                Ok((error, _)) => record_redis_error(&error, caller, "client"),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    // listen for unresponsive client events
    tokio::spawn(async move {
        loop {
            match unresponsive_rx.recv().await {
                Ok(server) => {
                    tracing::debug!("Redis client ({server:?}) unresponsive");
                    u64_counter_with_unit!(
                        "apollo.router.cache.redis.unresponsive",
                        "Counter for Redis client unresponsive events",
                        "{event}",
                        1,
                        kind = caller,
                        server = server.to_string()
                    );
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    // listen for reconnection events
    tokio::spawn(async move {
        loop {
            match reconnect_rx.recv().await {
                Ok(server) => {
                    u64_counter_with_unit!(
                        "apollo.router.cache.redis.reconnection",
                        "Counter for Redis client reconnection events",
                        "{reconnection}",
                        1,
                        kind = caller,
                        server = server.to_string()
                    );
                    tracing::info!("Redis client connected to {server:?}")
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl RedisCacheStorage {
    pub(crate) async fn truncate_namespace(&self) -> Result<(), RedisError> {
        use fred::prelude::Key;
        use futures::StreamExt;

        if self.namespace.is_none() {
            return Ok(());
        }

        // find all members of this namespace via `SCAN`
        let pattern = self.make_key(RedisKey("*"));
        let client = self.client().await?;
        let mut stream: Pin<Box<dyn Stream<Item = Result<Key, RedisError>>>> = if self.is_cluster {
            Box::pin(client.scan_cluster_buffered(pattern, None, None))
        } else {
            Box::pin(client.scan_buffered(pattern, None, None))
        };

        let mut keys = Vec::new();
        while let Some(key) = stream.next().await {
            keys.push(key?);
        }

        // remove all members of this namespace
        self.delete_from_scan_result(keys.into_iter()).await?;
        Ok(())
    }

    pub(crate) fn strip_namespace(&self, key: String) -> String {
        match &self.namespace {
            Some(namespace) => key
                .strip_prefix(&format!("{namespace}:"))
                .map(ToString::to_string)
                .unwrap_or(key),
            None => key,
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::SystemTime;

    use url::Url;

    use super::RedisCacheStorage;
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

    #[rstest::rstest]
    fn it_preprocesses_redis_schemes_correctly(
        #[values(
            "redis://username:password@host:6666/database",
            "rediss://username:password@host:6666/database",
            "redis-cluster://username:password@host:6666?node=host1:6667&node=host2:6668",
            "rediss-cluster://username:password@host:6666?node=host1:6667&node=host2:6668",
            "redis-sentinel://username:password@host:6666?node=host1:6667&node=host2:6668&sentinelServiceName=myservice&sentinelUserName=username2&sentinelPassword=password2",
            "rediss-sentinel://username:password@host:6666?node=host1:6667&node=host2:6668&sentinelServiceName=myservice&sentinelUserName=username2&sentinelPassword=password2"
        )]
        url: &str,
        #[values(1, 2, 3)] num_urls: usize,
    ) {
        let url = Url::parse(url).expect("invalid URL");
        let urls = vec![url; num_urls];

        let preprocess_result = RedisCacheStorage::preprocess_urls(urls);
        assert!(
            preprocess_result.is_ok(),
            "error = {:?}",
            preprocess_result.err()
        );
    }

    #[rstest::rstest]
    fn it_rejects_invalid_redis_scheme(
        #[values(
            "redis-invalid://username:password@host:6666/database",
            "other://username:password@host:6666/database"
        )]
        url: &str,
        #[values(1, 2, 3)] num_urls: usize,
    ) {
        let url = Url::parse(url).expect("invalid URL");
        let urls = vec![url; num_urls];

        let preprocess_result = RedisCacheStorage::preprocess_urls(urls);
        assert!(
            preprocess_result.is_err(),
            "error = {:?}",
            preprocess_result.err()
        );
    }

    #[rstest::rstest]
    #[case::same_scheme(
        "redis://username:password@host:6666/database",
        "redis://username:password@host:6666/database"
    )]
    #[case::one_cluster(
        "redis://username:password@host:6666/database",
        "redis-cluster://username:password@host:6666/database"
    )]
    fn it_preprocesses_redis_schemes_correctly_backwards_compatibility_valid_combinations(
        #[case] url1: &str,
        #[case] url2: &str,
    ) {
        let url1 = Url::parse(url1).expect("invalid URL");
        let url2 = Url::parse(url2).expect("invalid URL");

        // order shouldn't matter, so check both orders
        let url_pairings = [
            vec![url1.clone(), url2.clone()],
            vec![url2.clone(), url1.clone()],
        ];
        for url_pairing in url_pairings {
            let preprocess_result = RedisCacheStorage::preprocess_urls(url_pairing);
            assert!(
                preprocess_result.is_ok(),
                "error = {:?}",
                preprocess_result.err()
            );
        }
    }

    #[rstest::rstest]
    #[case(
        "redis://username:password@host:6666/database",
        "redis-sentinel://username:password@host:6666/database"
    )]
    #[case(
        "redis://username:password@host:6666/database",
        "rediss://username:password@host:6666/database"
    )]
    #[case(
        "redis-cluster://username:password@host:6666/database",
        "rediss://username:password@host:6666/database"
    )]
    #[case(
        "redis://username:password@host:6666/database",
        "rediss-sentinel://username:password@host:6666/database"
    )]
    // NB: this is not an exhaustive list, but it covers many common cases.
    fn it_preprocesses_redis_schemes_correctly_backwards_compatibility_invalid_combinations(
        #[case] url1: &str,
        #[case] url2: &str,
    ) {
        let url1 = Url::parse(url1).expect("invalid URL");
        let url2 = Url::parse(url2).expect("invalid URL");

        // order shouldn't matter, so check both orders
        let url_pairings = [
            vec![url1.clone(), url2.clone()],
            vec![url2.clone(), url1.clone()],
        ];
        for url_pairing in url_pairings {
            let preprocess_result = RedisCacheStorage::preprocess_urls(url_pairing.clone());
            assert!(preprocess_result.is_err(), "url pairing = {url_pairing:?}");
        }
    }

    /// Module that collects tests which actually run against Redis.
    ///
    /// This allows us to put the insanely long #[cfg] line in one place and fixes linting issues
    /// for unused imports.
    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    mod test_against_redis {
        use std::collections::HashMap;

        use fred::types::cluster::ClusterRouting;
        use itertools::Itertools;
        use rand::Rng as _;
        use rand::RngExt as _;
        use rand::distr::Alphanumeric;
        use serde_json::json;
        use tower::BoxError;
        use uuid::Uuid;

        use crate::cache::redis::RedisCacheStorage;
        use crate::cache::redis::RedisKey;
        use crate::cache::redis::RedisValue;

        fn random_namespace() -> String {
            Uuid::new_v4().to_string()
        }

        fn redis_config(clustered: bool) -> crate::configuration::RedisCache {
            let url = if clustered {
                "redis-cluster://localhost:7000"
            } else {
                "redis://localhost:6379"
            };

            let config_json = json!({
                "urls": [url],
                "namespace": random_namespace(),
                "required_to_start": true,
                "ttl": "60s"
            });

            serde_json::from_value(config_json).expect("invalid redis cache configuration")
        }

        async fn create_and_connect(clustered: bool) -> Result<RedisCacheStorage, BoxError> {
            Ok(RedisCacheStorage::new(redis_config(clustered), "test")
                .await?
                .create_client()
                .await?)
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn new_starts_empty_and_create_client_populates(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage = RedisCacheStorage::new(redis_config(clustered), "test").await?;
            assert!(storage.inner.read().is_none());

            let storage = storage.create_client().await?;
            assert!(storage.inner.read().is_some());
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn accessor_recreates_when_inner_is_none(
            #[values(true, false)] clustered: bool,
            #[values("client", "pipeline")] method: &str,
        ) -> Result<(), BoxError> {
            let storage = create_and_connect(clustered).await?;

            storage.inner.write().take();
            assert!(storage.inner.read().is_none());

            let first_call_failed = match method {
                "client" => storage.client().await.is_err(),
                "pipeline" => storage.pipeline().await.is_err(),
                _ => unreachable!(),
            };
            assert!(first_call_failed, "expected error during recreation");
            assert!(storage.inner.read().is_some());

            let second_call_ok = match method {
                "client" => storage.client().await.is_ok(),
                "pipeline" => storage.pipeline().await.is_ok(),
                _ => unreachable!(),
            };
            assert!(second_call_ok, "expected success after recreation");
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn client_works_after_recreation(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage = create_and_connect(clustered).await?;

            let key = RedisKey("recreation_test_key".to_string());
            storage
                .insert(key.clone(), RedisValue("before".to_string()), None)
                .await;

            storage.inner.write().take();
            let _ = storage.client().await;

            storage
                .insert(key.clone(), RedisValue("after".to_string()), None)
                .await;
            let fetched: RedisValue<String> = storage.get(key).await?;
            assert_eq!(fetched.0, "after");
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn recreation_stops_old_tasks_and_starts_new_ones(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage = create_and_connect(clustered).await?;

            // activate metrics so the abort handle is populated
            storage.activate();

            let (old_heartbeat, old_metrics) = {
                let guard = storage.inner.read();
                let inner = guard.as_ref().unwrap();
                (
                    inner.heartbeat_abort_handle.clone(),
                    inner.metrics_collector.abort_handle().expect("metrics not activated"),
                )
            };
            assert!(!old_heartbeat.is_finished());
            assert!(!old_metrics.is_finished());

            storage.inner.write().take();
            let _ = storage.client().await;

            assert!(old_heartbeat.is_finished());
            // old metrics task stops when the old DropSafeRedisPool is dropped
            assert!(old_metrics.is_finished());

            // activate the new inner's metrics
            storage.activate();

            let guard = storage.inner.read();
            let new_inner = guard.as_ref().unwrap();
            assert!(!new_inner.heartbeat_abort_handle.is_finished());
            let new_metrics = new_inner.metrics_collector.abort_handle().expect("metrics not activated");
            assert!(!new_metrics.is_finished());
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn clones_share_recreated_client(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage = create_and_connect(clustered).await?;
            let clone = storage.clone();

            storage.inner.write().take();
            assert!(clone.inner.read().is_none(), "clone should also see None");

            let _ = storage.client().await;
            assert!(
                clone.inner.read().is_some(),
                "clone should see the recreated client"
            );
            assert!(clone.client().await.is_ok());
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn create_client_replaces_existing_inner(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage = create_and_connect(clustered).await?;

            let old_heartbeat = storage
                .inner
                .read()
                .as_ref()
                .unwrap()
                .heartbeat_abort_handle
                .clone();
            assert!(!old_heartbeat.is_finished());

            let storage = storage.create_client().await?;
            tokio::task::yield_now().await;

            assert!(old_heartbeat.is_finished());
            assert!(storage.inner.read().is_some());
            assert!(storage.client().await.is_ok());
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        #[rstest::rstest]
        async fn concurrent_recreation_does_not_panic_or_deadlock(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage = create_and_connect(clustered).await?;
            storage.inner.write().take();

            let handles: Vec<_> = (0..10)
                .map(|_| {
                    let s = storage.clone();
                    tokio::spawn(async move { s.client().await })
                })
                .collect();

            for result in futures::future::join_all(handles).await {
                assert!(result.is_ok(), "task panicked");
            }
            assert!(storage.inner.read().is_some());
            assert!(storage.client().await.is_ok());
            Ok(())
        }

        /// scan_with_namespaced_results should work after client recreation
        #[tokio::test]
        #[rstest::rstest]
        async fn scan_works_after_recreation(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            use fred::types::scan::Scanner;
            use futures::StreamExt;

            let storage = create_and_connect(clustered).await?;
            let key = RedisKey("scan_test_key".to_string());
            storage
                .insert(key, RedisValue("value".to_string()), None)
                .await;

            storage.inner.write().take();
            let _ = storage.client().await;

            let mut stream = storage
                .scan_with_namespaced_results("*".to_string(), Some(100))
                .await?;
            let mut found_keys = Vec::new();
            while let Some(result) = stream.next().await {
                if let Some(keys) = result?.take_results() {
                    found_keys.extend(keys);
                }
            }
            assert!(!found_keys.is_empty(), "scan should find at least one key");
            Ok(())
        }

        /// Tests that `insert_multiple` and `get_multiple` are successful when run against clustered Redis.
        ///
        /// Clustered Redis works by hashing each key to one of 16384 hash slots, and assigning each hash
        /// slot to a node. Operations which interact with multiple keys (`MGET`, `MSET`) *cannot* be
        /// used on keys which map to different hash slots, even if those hash slots are on the same node.
        ///
        /// This test inserts data that is guaranteed to hash to different slots to verify that
        /// `RedisCacheStorage` is well-behaved when operating against a cluster.
        #[tokio::test(flavor = "multi_thread")]
        async fn test_redis_storage_avoids_common_cross_slot_errors() -> Result<(), BoxError> {
            let clustered = true;
            let storage = RedisCacheStorage::new(redis_config(clustered), "test_redis_storage")
                .await?
                .create_client()
                .await?;

            // insert values which reflect different cluster slots
            let mut data = HashMap::default();
            let expected_value = rand::rng().next_u32() as usize;
            let unique_cluster_slot_count = |data: &HashMap<RedisKey<String>, _>| {
                data.keys()
                    .map(|key| ClusterRouting::hash_key(key.0.as_bytes()))
                    .unique()
                    .count()
            };

            while unique_cluster_slot_count(&data) < 50 {
                // NB: include {} around key so that this key is what determines the cluster hash slot - adding
                // the namespace will otherwise change the slot
                let key = rand::rng()
                    .sample_iter(&Alphanumeric)
                    .take(10)
                    .map(char::from)
                    .collect::<String>();
                data.insert(RedisKey(format!("{{{}}}", key)), RedisValue(expected_value));
            }

            // insert values
            let keys: Vec<_> = data.keys().cloned().collect();
            let data: Vec<_> = data.into_iter().collect();
            let _ = storage.insert_multiple(&data, None).await;

            // make a `get` call for each key and ensure that it has the expected value. this tests both
            // the `get` and `insert_multiple` functions
            for key in &keys {
                let value: RedisValue<usize> = storage.get(key.clone()).await?;
                assert_eq!(value.0, expected_value);
            }

            // test the `mget` functionality
            let values = storage.get_multiple(keys).await?;
            for value in values {
                let value: RedisValue<usize> = value?;
                assert_eq!(value.0, expected_value);
            }

            Ok(())
        }

        /// Test that `get_multiple` returns items in the correct order.
        #[tokio::test]
        #[rstest::rstest]
        async fn test_get_multiple_is_ordered(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage =
                RedisCacheStorage::new(redis_config(clustered), "test_get_multiple_is_ordered")
                    .await?
                    .create_client()
                    .await?;

            let data = [("a", "1"), ("b", "2"), ("c", "3")]
                .map(|(k, v)| (RedisKey(k.to_string()), RedisValue(v.to_string())));
            let _ = storage.insert_multiple(&data, None).await;

            // check different orders of fetches to make everything is ordered correctly, including
            // when some values are none
            let test_cases = vec![
                (vec!["a", "b", "c"], vec![Some("1"), Some("2"), Some("3")]),
                (vec!["c", "b", "a"], vec![Some("3"), Some("2"), Some("1")]),
                (vec!["d", "b", "c"], vec![None, Some("2"), Some("3")]),
                (
                    vec!["d", "3", "s", "b", "s", "1", "c", "Y"],
                    vec![None, None, None, Some("2"), None, None, Some("3"), None],
                ),
            ];

            for (keys, expected_values) in test_cases {
                let keys: Vec<RedisKey<_>> = keys
                    .into_iter()
                    .map(|key| RedisKey(key.to_string()))
                    .collect();
                let expected_values: Vec<Option<String>> = expected_values
                    .into_iter()
                    .map(|value| value.map(ToString::to_string))
                    .collect();

                let values = storage.get_multiple(keys).await?;
                let parsed_values: Vec<Option<String>> =
                    values.into_iter().map(|v| v.ok().map(|v| v.0)).collect();
                assert_eq!(parsed_values, expected_values);
            }

            Ok(())
        }
    }
}
