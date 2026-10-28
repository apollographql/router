use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
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
use fred::types::config::ReplicaConfig;
use fred::types::config::TlsConfig;
use fred::types::config::TlsHostMapping;
use fred::types::config::UnresponsiveConfig;
use futures::future::join_all;
use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::AbortHandle;
use tokio::time::Instant;
use tokio::time::MissedTickBehavior;
use tower::BoxError;
use url::Url;

use super::KeyType;
use super::ValueType;
use super::metrics::RedisMetricsCollector;
use crate::cache::replica_filter::RouteableReplicaFilter;
use crate::configuration::RedisCache;
use crate::services::subgraph::http::create_certificate_store;
use crate::services::subgraph::http::generate_tls_client_config;

#[cfg(test)]
pub(super) mod fake_redis;

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
/// Sentinel argument sent with the replica keep-alive `PING`. Redis echoes it back and it shows up
/// verbatim in `MONITOR`, so the ping is attributable to us when debugging (and in tests).
const REPLICA_HEARTBEAT_PING_MSG: &str = "apollo-router-replica-heartbeat";

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
    // Keep-alive for replica connections; only present in cluster mode, where the router
    // establishes replica connections. `None` otherwise.
    replica_heartbeat_abort_handle: Option<AbortHandle>,
    // The replica filter the pool's clients share, so tests can run the replica heartbeat.
    #[cfg(test)]
    replica_filter: Option<Arc<RouteableReplicaFilter>>,
    watcher_abort_handle: AbortHandle,
    // Held so the metrics polling task is aborted (via the collector's Drop) when the
    // pool is dropped.
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
        let caller = self.caller;
        self.heartbeat_abort_handle.abort();
        if let Some(handle) = &self.replica_heartbeat_abort_handle {
            handle.abort();
        }
        self.watcher_abort_handle.abort();

        tokio::spawn(async move {
            let clients_aborted = inner.clients().len() as u64;
            let _ = inner
                .quit()
                .await
                .inspect_err(|err| record_redis_error(err, caller, "shutdown"));
            ACTIVE_CLIENT_COUNT.fetch_sub(clients_aborted, Ordering::Relaxed);
        });
        // Metrics collector will be dropped automatically and its Drop impl will abort the task
    }
}

/// Configuration wrapping the redis client configuration; includes useful metadata like whether
/// we're using clustered redis, but also carries the actual redis connection poll
#[derive(Clone)]
pub(crate) struct RedisCacheStorage {
    // we use a lock for its interior mutability, don't confuse this lock for a lock on the redis
    // commands (eg, by thinking you need to take a write lock out before doing a redis write command)
    //
    // NOTE: we use the parking_lot::RwLock rather than the tokio::sync::RwLock because we don't do
    // any asynchronous work for the lock: we acquire it, read the pool, and clone the client; only
    // after that do we the asynchronous work of talking to redis. tokio::sync::RwLock introduces
    // all the normal asynchronous machinery that we want to avoid here for its overhead
    inner: Arc<RwLock<Option<DropSafeRedisPool>>>,
    namespace: Option<Arc<String>>,
    pub(crate) ttl: Option<Duration>,
    is_cluster: bool,
    reset_ttl: bool,
    // the wrapped client config, comes from the router config
    redis_client_config: RedisClientConfig,
    pool_recreation_lock: Arc<Mutex<()>>,
}

/// Configuration for the redis client, pulled from fields of the router config
#[derive(Clone)]
struct RedisClientConfig {
    client_config: RedisConfig,
    timeout: Duration,
    pool_size: usize,
    caller: &'static str,
    metrics_interval: Duration,
    required_to_start: bool,
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
    /// Create a new RedisCacheStorage without an inner client. To create an inner client, you must call `create_client_pool`
    pub(crate) async fn new(config: RedisCache, caller: &'static str) -> Result<Self, BoxError> {
        let url = Self::preprocess_urls(config.urls)
            .inspect_err(|err| record_redis_error(err, caller, "startup"))?;
        let mut client_config = RedisConfig::from_url(url.as_str())
            .inspect_err(|err| record_redis_error(err, caller, "startup"))?;
        let is_cluster = client_config.server.is_clustered();

        if let Some(username) = config.username {
            client_config.username = Some(username.unredact().clone());
        }

        if let Some(password) = config.password {
            client_config.password = Some(password.unredact().clone());
        }

        if let Some(tls) = config.tls.as_ref() {
            let tls_cert_store = create_certificate_store(tls).transpose()?;
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
            pool_recreation_lock: Arc::new(Mutex::new(())),
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
            pool_recreation_lock: Arc::new(Mutex::new(())),
        };

        storage.create_client_pool().await?;
        Ok(storage)
    }

    /// Initializes an inner client pool
    pub(crate) async fn create_client_pool(&self) -> Result<(), BoxError> {
        let replica_filter = self.is_cluster.then(|| {
            Arc::new(RouteableReplicaFilter::new(
                self.redis_client_config.client_config.clone(),
                self.redis_client_config.timeout,
            ))
        });
        let client_pool = Builder::from_config(self.redis_client_config.client_config.clone())
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
                // WARN: replica settings must only be applied in cluster mode; that's the only
                // mode where we route commands to replicas (see `get`, `get_multiple` and
                // `ping_replicas`). Outside of a cluster, fred still discovers replicas via `ROLE`
                // on the primary, and with eager connections and fatal reconnection errors a
                // replica we never use that accepts connections but never answers (a paused
                // replica, or an HA standby that doesn't accept clients) fails every connection
                // attempt, aborts the client and recreates the whole pool, taking the primary down
                // with it. Non-cluster pools keep fred's defaults: lazy replica connections that
                // are never opened because nothing is routed to them.
                if let Some(filter) = &replica_filter {
                    configure_cluster_replicas(&mut config.replica, filter.clone());
                }
                config.tcp = TcpConfig {
                    #[cfg(target_os = "linux")]
                    user_timeout: Some(self.redis_client_config.timeout),
                    ..Default::default()
                };
                config.unresponsive = UnresponsiveConfig {
                    max_timeout: Some(DEFAULT_INTERNAL_REDIS_TIMEOUT),
                    interval: Duration::from_secs(2),
                };
            })
            .with_performance_config(|config| {
                config.default_command_timeout = self.redis_client_config.timeout;
            })
            // NOTE: we give fred 15 attempts to reconnect; once reconnected, fred resets its
            // counter and we have another 15 attempts. If reconnection fails, fred aborts and _we_
            // recreate the redis client. Currently, we attempt recreation indefinitely
            .set_policy(ReconnectPolicy::new_exponential(15, 1, 2000, 5))
            .build_pool(self.redis_client_config.pool_size)?;

        for client in client_pool.clients() {
            setup_event_listeners(self.redis_client_config.caller, client);
            ACTIVE_CLIENT_COUNT.fetch_add(1, Ordering::Relaxed);
        }

        // When `required_to_start` is set, subscribe to each client's `connect`
        // notification *before* kicking off connection tasks. fred's
        // `Pool::wait_for_connect` subscribes only at the moment it is `.await`ed,
        // so there is a race: if `connect_pool` exhausts its reconnection policy
        // before `wait_for_connect` reaches the `recv()` call, the terminal
        // `broadcast_connect(Err(..))` is fired before any subscriber exists. The
        // broadcast channel does not buffer past events for late subscribers, and
        // fred never sends another connect event (the pool has aborted), so
        // `wait_for_connect` hangs indefinitely. We saw this surface in CI as the
        // `connection_failure_blocks_startup` integration test timing out at the
        // nextest 120 s deadline.
        //
        // We mirror fred's own `Pool::init` shape (clients/pool.rs `init`) which
        // collects subscribers first and then calls `connect`, but keep the
        // separate per-client `ConnectHandle`s that the downstream watcher task
        // (see below) needs to detect a pool abort. This is a subscribe-before-
        // publish fix; the broader connection lifecycle is unchanged.
        let connect_rxs: Vec<_> = if self.redis_client_config.required_to_start {
            client_pool
                .clients()
                .iter()
                .map(|c| c.inner().notifications.connect.load().subscribe())
                .collect()
        } else {
            Vec::new()
        };

        // NB: error is not recorded here as it will be observed by the task following `client.error_rx()`
        let client_handles = client_pool.connect_pool();
        if self.redis_client_config.required_to_start {
            // Drive each pre-subscribed receiver to its first event. If any
            // client's connect attempts ultimately fail, surface that error so the
            // router can refuse to start (the contract documented on
            // `required_to_start`).
            for mut rx in connect_rxs {
                loop {
                    match rx.recv().await {
                        Ok(Ok(())) => break,
                        Ok(Err(e)) => return Err(Box::new(e)),
                        Err(RecvError::Lagged(_)) => {
                            // A lag means the broadcast queue overflowed between
                            // the subscribe and recv; this only happens under
                            // extreme load and is not a connect failure, so
                            // re-recv on the SAME rx to wait for the next event.
                            // Without the inner loop, `continue` would advance
                            // the outer `for` and drop this client's receiver
                            // without ever observing its connect.
                            continue;
                        }
                        Err(RecvError::Closed) => {
                            return Err(Box::new(RedisError::new(
                                RedisErrorKind::Canceled,
                                "redis connect notification channel closed before any connect attempt completed",
                            )));
                        }
                    }
                }
            }
            tracing::trace!("redis connections established");
        }

        // We spawn a task for watching pool shutdown; shutdown can happen either when reconnection
        // attempts are exhausted or, if configured to never stop trying to reconnect, when quit()
        // is called by us (it's never called within fred)
        //
        // Don't mistake connections for clients. This is a pool of clients that handles
        // connections, and if a connection breaks, fred will internally (attempt to) recreate it.
        // Configuration for that is governed below in the ReconnectPolicy struct
        //
        // WARN: we need a weak reference to the data (ie, Weak), not one that keeps the data around
        // as long as it's being referenced (ie, Arc). This matters because when a config reload
        // happens via hot-reload and the watcher holds a strong reference (Arc), the total
        // refcount can be 2 for the previous config's pool; when RedisCacheStorage is dropped,
        // that refcount goes down to 1, but that's not 0--it leaks the old pool. When using a weak
        // reference, we don't increment the refcount by 1 for what's held in the watcher; rather,
        // we have to upgrade() to see if that data still exists; this means that we can drop the
        // RedisCacheStorage from the previous config without leaking it
        let client_pool_downgraded = Arc::downgrade(&self.inner);
        let watcher_handle = tokio::spawn(async move {
            // WARN: the client_handles returning is the signal that fred has been aborted; don't
            // remove it!
            let _fred_aborted = join_all(client_handles).await;
            if let Some(inner) = client_pool_downgraded.upgrade() {
                inner.write().take();
                tracing::info!("redis client aborted; marking for recreation");
            }
        });

        let client_heartbeats = client_pool.clone();
        let heartbeat_handle = tokio::spawn(async move {
            client_heartbeats
                .enable_heartbeat(REDIS_HEARTBEAT_INTERVAL, false)
                .await
        });

        // fred's heartbeat above only reaches the primary connection (its PING doesn't set
        // `use_replica`). In cluster mode we also hold eager replica connections, so we run a
        // second heartbeat to keep those sockets warm and prevent server-side idle reaping, and to
        // stop routing to replicas that stop answering.
        let replica_heartbeat_handle = replica_filter.clone().map(|filter| {
            let client_heartbeats = client_pool.clone();
            let caller = self.redis_client_config.caller;
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval_at(
                    Instant::now() + REDIS_HEARTBEAT_INTERVAL,
                    REDIS_HEARTBEAT_INTERVAL,
                );
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

                loop {
                    ticker.tick().await;
                    ping_replicas(&client_heartbeats, &filter, caller).await;
                }
            })
        });

        let pooled_client_arc = Arc::new(client_pool);
        let metrics_collector = RedisMetricsCollector::new(
            pooled_client_arc.clone(),
            self.redis_client_config.caller,
            self.redis_client_config.metrics_interval,
        );

        let inner = DropSafeRedisPool {
            pool: pooled_client_arc,
            caller: self.redis_client_config.caller,
            heartbeat_abort_handle: heartbeat_handle.abort_handle(),
            replica_heartbeat_abort_handle: replica_heartbeat_handle
                .as_ref()
                .map(|h| h.abort_handle()),
            #[cfg(test)]
            replica_filter,
            watcher_abort_handle: watcher_handle.abort_handle(),
            _metrics_collector: metrics_collector,
        };

        // replace the current pool (if there is one) with the new one
        *self.inner.write() = Some(inner);

        Ok(())
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

    // NOTE: we return a RedisError here because it's easier to integrate into the rest of the
    // system (we use it everywhere), but if we refactor this file in order to replace our current
    // redis client with a different one, we should use our own error rather than use a library's
    // to avoid this kind of thing in the future
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
                let guard = if let Ok(lock) = self.pool_recreation_lock.clone().try_lock_owned() {
                    lock
                } else {
                    let error = RedisError::new(
                        RedisErrorKind::Unknown,
                        "Error: attempting to send a command to Redis while its client pool is recreating",
                    );
                    record_redis_error(&error, self.redis_client_config.caller, "client");
                    return Err(error);
                };
                // WARN: don't remove this; this makes sure that once we have a lock, we aren't
                // recreating the client. Multiple recreations can happen if we queue up tasks
                // waiting for locks and we need to make sure that after we've acquired a lock, we
                // aren't just recreating for no reason
                if let Some(client) = self.inner.read().as_ref() {
                    let client = client.next().clone();
                    return Ok(client);
                }

                let cloned_self = self.clone();
                tokio::task::spawn(async move {
                    // make sure we only spawn one background task at a time for pool recreation
                    let _guard = guard;
                    // this will attempt to recreate the client pool on the next command, so we
                    // don't do any special retry logic here; we just record failures
                    if let Err(e) = cloned_self.create_client_pool().await {
                        let error = RedisError::new(
                            RedisErrorKind::Unknown,
                            format!("Error attempting to recreate client: {e:?}"),
                        );
                        record_redis_error(
                            &error,
                            cloned_self.redis_client_config.caller,
                            "client",
                        );
                    }
                });

                // rather than get into either recursion or a loop, we just return an error and let
                // the current attempt to reach redis fail. We have a new client waiting for the
                // next attempt, so this should be a temporary failure
                let error = RedisError::new(
                    RedisErrorKind::Unknown,
                    "client pool being recreated after connection interrupt",
                );
                record_redis_error(&error, self.redis_client_config.caller, "client");
                Err(error)
            }
        }
    }

    pub(crate) async fn pipeline(&self) -> Result<Pipeline<Client>, RedisError> {
        Ok(self.client().await?.pipeline())
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
        // NOTE: we've already recorded client failure errors in client(), so using if let Ok()
        // rather than duplicating reporting/handling
        if let Ok(client) = self.client().await {
            let result: Result<(), _> = client
                .set(key, value, self.expiration(ttl), None, false)
                .await;

            tracing::trace!("insert result {:?}", result);
            if let Err(err) = result {
                self.record_query_error(&err);
            }
        }
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
}

/// Replica settings for cluster mode, where reads are routed to replicas.
///
/// With eager connections and fatal reconnection errors, fred fails the whole client if it can't
/// connect to a replica it routes to. The [`RouteableReplicaFilter`] prevents that by probing each
/// replica before fred connects to it, and leaving out the ones that can't complete a connection.
fn configure_cluster_replicas(replica: &mut ReplicaConfig, filter: Arc<RouteableReplicaFilter>) {
    // NOTE: this defaults to true in fred but we're intentionally paying attention to
    // errors because sometimes fred gets into a funky state with its redis
    // connections. Sometimes, eg, a brief network outage in whatever cloud provider folks are
    // using can destabilize what fred views as routeable nodes--ignoring errors can
    // get us into a state where fred can't connect to those nodes and we don't know
    // that we're unable to connect to them; it's much better to let fred abort and
    // then recreate the client with a fresh view of the infrastructure
    replica.ignore_reconnection_errors = false;
    replica.primary_fallback = true;
    // NOTE: lazy_connections must be false! There's a bug in fred's round-robin logic
    // for selecting replicas when in a cluster that has an even number of replicas.
    // The details are roughly that when fred looks for a routeable replica, it
    // increments its round-robin counter, and if it can't find one, it moves on to
    // creating one but also increments the counter _again_ before requeuing the redis
    // command to run. When there are, eg, 2 replicas, those requeues will make it so
    // that fred always tries to route a command to a replica that has no connection.
    // Redis CPU explodes, GETs fail and fallthrough to backends, and so on. Not great.
    // Don't set this to true! We originally set it to true to have additional blast
    // radius for sync failures but we now have a ReplicaFilter doing that work for us
    // (by removing unrouteable replicas from what we try to route to)
    replica.lazy_connections = false;
    replica.filter = Some(filter);
}

/// Send a keep-alive `PING` to the replica connections held by the pool, so eager replica sockets
/// don't idle out (see [`REPLICA_HEARTBEAT_PING_MSG`]).
///
/// fred can't target a replica directly; it round-robins across a primary's replicas. So we send
/// one ping per replica and let the round-robin spread them out. Interleaving traffic shares that
/// counter and can skew a round (one replica pinged twice, another skipped), but that only happens
/// when replica traffic is flowing and already keeping the sockets warm. When idle -- the case that
/// causes reaping -- our N pings cover all N replicas.
///
/// When a ping fails, the client's replicas are re-synced, so the [`RouteableReplicaFilter`] probes
/// each one again and leaves out any that stopped answering. fred only notices an unresponsive
/// connection once nothing has been written to it for `UnresponsiveConfig::max_timeout`, which
/// steady traffic prevents, so without this a replica that stalls after connecting keeps receiving
/// (and timing out) its share of reads.
///
/// One failed ping is enough to re-sync, which has a cost: fred re-syncs inside the client's
/// router task, so the client's other commands wait while it probes each replica and reopens its
/// connection to every one that passes. A stalled replica holds that up for one Redis `timeout`.
/// Every client whose ping failed re-syncs, but they share one probe of each replica.
///
/// A left-out replica isn't pinged, since no client routes to it, so nothing else would bring it
/// back once it recovers. Once its left-out period is up, the filter forgets it and every client
/// re-syncs once, which probes it again. A replica that's still broken is left out again, so it
/// costs one Redis `timeout` and one re-sync of every client per period. A replica the cluster no
/// longer lists costs one re-sync of every client, after which it's forgotten.
async fn ping_replicas(pool: &RedisPool, filter: &RouteableReplicaFilter, caller: &'static str) {
    // Taken once for the whole pool, since every client must re-sync to route to the replica again.
    let recheck = filter.take_replicas_to_recheck();
    join_all(
        pool.clients()
            .iter()
            .map(|client| ping_client_replicas(client, recheck, caller)),
    )
    .await;
}

async fn ping_client_replicas(client: &Client, recheck: bool, caller: &'static str) {
    // `nodes()` maps each replica server to its primary.
    let pings = client.replicas().nodes().into_values().map(|primary| {
        let options = Options {
            cluster_node: Some(primary),
            fail_fast: true,
            ..Default::default()
        };
        async move {
            client
                .replicas()
                .with_options(&options)
                .ping::<()>(Some(REPLICA_HEARTBEAT_PING_MSG.to_string()))
                .await
        }
    });

    let mut all_answered = true;
    for result in join_all(pings).await {
        if let Err(err) = result {
            // Keep-alive is best-effort; log and move on. We don't record this as a query
            // error since it doesn't correspond to a user-facing cache operation.
            tracing::info!(caller = caller, error = ?err, "replica heartbeat ping failed");
            all_answered = false;
        }
    }

    if (!all_answered || recheck)
        && let Err(err) = client.replicas().sync(false).await
    {
        tracing::info!(
            caller = caller,
            error = ?err,
            "failed to re-sync replicas at the replica heartbeat"
        );
    }
}

/// Sets up the error, reconnection, and unresponsive event listeners
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
        use std::pin::Pin;

        use fred::prelude::Key;
        use futures::Stream;
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

    pub(crate) async fn get_multiple<K: KeyType, V: ValueType>(
        &self,
        keys: Vec<RedisKey<K>>,
    ) -> Result<Vec<Result<RedisValue<V>, RedisError>>, RedisError> {
        self.get_multiple_with_options(keys, Options::default())
            .await
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

    /// The keys returned in `ScanResult` do include the prefix from `namespace` configuration.
    pub(crate) async fn scan_with_namespaced_results(
        &self,
        pattern: String,
        count: Option<u32>,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = Result<fred::types::scan::ScanResult, RedisError>>
                    + Send,
            >,
        >,
        RedisError,
    > {
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

    /// Cluster pools must keep the eager, strictly monitored replica connections that work around
    /// fred's round-robin bug with an even number of replicas; every other pool keeps fred's
    /// defaults. Checks the settings of the client the pool was built with, so it fails if
    /// `create_client_pool` stops applying them or applies them regardless of topology.
    #[rstest::rstest]
    #[case::cluster("redis-cluster", true)]
    #[case::standalone("redis", false)]
    #[tokio::test]
    async fn replica_settings_apply_only_to_cluster_pools(
        #[case] scheme: &str,
        #[case] cluster: bool,
    ) {
        use fred::prelude::ClientLike;
        use fred::types::config::ReplicaConfig;

        // nothing listens here; the pool is built without waiting for a connection
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = serde_json::from_value(serde_json::json!({
            "urls": [format!("{scheme}://127.0.0.1:{port}")],
            "required_to_start": false,
        }))
        .unwrap();
        let storage = RedisCacheStorage::new(config, "test").await.unwrap();
        storage.create_client_pool().await.unwrap();

        let client = storage.client().await.unwrap();
        let replica = &client.connection_config().replica;
        assert_eq!(storage.is_cluster, cluster);
        if cluster {
            assert!(!replica.lazy_connections);
            assert!(!replica.ignore_reconnection_errors);
            assert!(replica.primary_fallback);
            assert!(replica.filter.is_some());
        } else {
            assert_eq!(replica, &ReplicaConfig::default());
            assert!(replica.filter.is_none());
        }
    }

    /// Runs the storage against in-process fake deployments of each topology, with one replica
    /// misbehaving in each way replicas fail in production, and checks that the pool stays usable
    /// and that reads go to the right nodes.
    mod topology_matrix {
        use std::time::Duration;

        use serde_json::json;
        use tower::BoxError;

        use crate::cache::redis::DEFAULT_INTERNAL_REDIS_TIMEOUT;
        use crate::cache::redis::RedisCacheStorage;
        use crate::cache::redis::RedisKey;
        use crate::cache::redis::RedisValue;
        use crate::cache::redis::fake_redis::Behaviour;
        use crate::cache::redis::fake_redis::FakeRedis;
        use crate::cache::redis::fake_redis::NodeStats;
        use crate::cache::redis::fake_redis::Topology;

        /// Half the internal command timeout, so a client that waits on a broken replica while
        /// connecting, or keeps retrying a command, misses it.
        const DEADLINE: Duration =
            Duration::from_secs(DEFAULT_INTERNAL_REDIS_TIMEOUT.as_secs() / 2);

        const CONNECT_STALLED: &str = "connecting should not wait on a replica";

        async fn connect(redis: &FakeRedis) -> Result<RedisCacheStorage, BoxError> {
            connect_with(redis, json!({})).await
        }

        /// Connects with the fields of `overrides` replacing the matrix's defaults.
        async fn connect_with(
            redis: &FakeRedis,
            overrides: serde_json::Value,
        ) -> Result<RedisCacheStorage, BoxError> {
            let mut config = json!({
                "urls": [redis.url],
                "required_to_start": true,
                "timeout": "1s",
            });
            if let (Some(config), Some(overrides)) = (config.as_object_mut(), overrides.as_object())
            {
                config.extend(overrides.clone());
            }
            let storage = RedisCacheStorage::new(serde_json::from_value(config)?, "test").await?;
            tokio::time::timeout(DEADLINE, storage.create_client_pool())
                .await
                .map_err(|_| CONNECT_STALLED)??;
            Ok(storage)
        }

        /// Writes and reads back through the single and multiple key paths.
        async fn round_trip(storage: &RedisCacheStorage) -> Result<(), BoxError> {
            let commands = async {
                let single = RedisKey("single".to_string());
                let multiple = RedisKey("multiple".to_string());
                storage
                    .insert(single.clone(), RedisValue("one".to_string()), None)
                    .await;
                storage
                    .insert_multiple(&[(multiple.clone(), RedisValue("two".to_string()))], None)
                    .await?;

                let value: RedisValue<String> = storage.get(single.clone()).await?;
                assert_eq!(value.0, "one");
                let values = storage
                    .get_multiple::<_, String>(vec![single, multiple])
                    .await?
                    .into_iter()
                    .map(|value| value.map(|value| value.0))
                    .collect::<Result<Vec<_>, _>>()?;
                assert_eq!(values, ["one", "two"]);
                Ok(())
            };
            tokio::time::timeout(DEADLINE, commands)
                .await
                .map_err(|_| "commands should not wait on a replica")?
        }

        /// Runs one round of the replica heartbeat, as the pool's heartbeat task does every
        /// `REDIS_HEARTBEAT_INTERVAL`.
        async fn ping_replicas(storage: &RedisCacheStorage) {
            let (pool, filter) = {
                let inner = storage.inner.read();
                let inner = inner.as_ref().expect("pool");
                (inner.pool.clone(), inner.replica_filter.clone())
            };
            let filter = filter.expect("a cluster pool has a replica filter");
            crate::cache::redis::ping_replicas(&pool, &filter, "test").await;
        }

        /// Ends the time left-out replicas must wait before they're probed again, as if it had
        /// passed.
        fn end_left_out_period(storage: &RedisCacheStorage) {
            storage
                .inner
                .read()
                .as_ref()
                .and_then(|inner| inner.replica_filter.as_ref())
                .expect("a cluster pool has a replica filter")
                .end_left_out_period();
        }

        fn assert_pool_kept(storage: &RedisCacheStorage) {
            assert!(
                storage.inner.read().is_some(),
                "the pool should not have been marked for recreation"
            );
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn pool_without_replicas_serves_commands(
            #[values(Topology::Centralized, Topology::Sentinel, Topology::Cluster)]
            topology: Topology,
        ) -> Result<(), BoxError> {
            let redis = FakeRedis::start(topology, &[]).await;
            let storage = connect(&redis).await?;
            round_trip(&storage).await?;

            assert!(storage.inner.read().is_some());
            assert!(redis.primary.reads() > 0);
            Ok(())
        }

        /// The first replica behaves as given and any others are healthy.
        ///
        /// Outside of a cluster, the router never reads from replicas, so it must never connect
        /// to one (from 2.15.1 it connected eagerly and treated replica errors as fatal, so a
        /// replica that never answered recreated the pool in a loop). In a cluster, reads go to
        /// healthy replicas, falling back to the primary when there are none (with lazy replica
        /// connections, fred's round-robin never reached a connected replica when there were two,
        /// and reads retried forever). A cluster replica that can't serve commands is left out
        /// rather than failing the connection.
        #[tokio::test]
        #[rstest::rstest]
        async fn pool_stays_usable_and_routes_reads(
            #[values(Topology::Centralized, Topology::Sentinel, Topology::Cluster)]
            topology: Topology,
            #[values(1, 2)] replica_count: usize,
            #[values(
                Behaviour::Healthy,
                Behaviour::Refusing,
                Behaviour::Dropping,
                Behaviour::Unresponsive
            )]
            first_replica: Behaviour,
        ) -> Result<(), BoxError> {
            let mut behaviours = vec![Behaviour::Healthy; replica_count];
            behaviours[0] = first_replica;
            let redis = FakeRedis::start(topology, &behaviours).await;

            let storage = connect(&redis).await?;
            round_trip(&storage).await?;

            assert!(
                storage.inner.read().is_some(),
                "the pool should not have been marked for recreation"
            );
            let healthy_replica_reads: usize = redis.healthy_replicas().map(NodeStats::reads).sum();
            if !topology.is_cluster() {
                assert_eq!(
                    redis.replica_connections(),
                    0,
                    "a non-cluster pool should never connect to a replica"
                );
                assert!(redis.primary.reads() > 0);
            } else if redis.healthy_replicas().next().is_some() {
                assert_eq!(redis.primary.reads(), 0, "reads should go to replicas");
                assert!(healthy_replica_reads > 0);
            } else {
                assert!(
                    redis.primary.reads() > 0,
                    "reads should fall back to the primary"
                );
            }
            Ok(())
        }

        /// A cluster replica that stops answering after the pool connected is left out at the
        /// next replica heartbeat, rather than timing out its share of reads until fred notices.
        #[tokio::test]
        async fn replica_that_stalls_is_left_out_at_the_next_heartbeat() -> Result<(), BoxError> {
            let redis = FakeRedis::start(Topology::Cluster, &[Behaviour::Healthy]).await;
            let replica = &redis.replicas[0].stats;
            let storage = connect(&redis).await?;
            round_trip(&storage).await?;
            assert!(replica.reads() > 0, "reads should go to the replica");

            replica.stall();
            ping_replicas(&storage).await;

            let primary_reads = redis.primary.reads();
            round_trip(&storage).await?;
            assert!(
                redis.primary.reads() > primary_reads,
                "reads should fall back to the primary"
            );
            assert_pool_kept(&storage);
            Ok(())
        }

        /// When one of two cluster replicas stalls, reads move to the other replica rather than
        /// to the primary.
        #[tokio::test]
        async fn reads_move_to_the_other_replica_when_one_stalls() -> Result<(), BoxError> {
            let redis =
                FakeRedis::start(Topology::Cluster, &[Behaviour::Healthy, Behaviour::Healthy])
                    .await;
            let (stalled, healthy) = (&redis.replicas[0].stats, &redis.replicas[1].stats);
            let storage = connect(&redis).await?;

            stalled.stall();
            ping_replicas(&storage).await;

            let primary_reads = redis.primary.reads();
            let healthy_reads = healthy.reads();
            round_trip(&storage).await?;
            assert!(
                healthy.reads() > healthy_reads,
                "reads should go to the healthy replica"
            );
            assert_eq!(
                redis.primary.reads(),
                primary_reads,
                "reads should stay off the primary"
            );
            assert_pool_kept(&storage);
            Ok(())
        }

        /// When a replica stalls, every client in the pool fails its heartbeat ping and re-syncs at
        /// once. They share one probe of the stalled replica, so it gets one new connection
        /// rather than one per client.
        #[tokio::test]
        async fn pool_clients_share_one_probe_of_a_stalled_replica() -> Result<(), BoxError> {
            let redis = FakeRedis::start(Topology::Cluster, &[Behaviour::Healthy]).await;
            let replica = &redis.replicas[0].stats;
            let storage = connect_with(&redis, json!({ "pool_size": 4 })).await?;

            replica.stall();
            let connections = replica.connections();
            ping_replicas(&storage).await;
            assert_eq!(
                replica.connections() - connections,
                1,
                "the clients should share one probe"
            );

            let primary_reads = redis.primary.reads();
            round_trip(&storage).await?;
            assert!(
                redis.primary.reads() > primary_reads,
                "reads should fall back to the primary"
            );
            assert_pool_kept(&storage);
            Ok(())
        }

        /// A replica left out after it stalled gets reads again once it recovers: when its left-out
        /// period ends, the next heartbeat re-syncs every client, so each one probes it again.
        #[tokio::test]
        async fn stalled_replica_gets_reads_again_once_it_recovers() -> Result<(), BoxError> {
            let redis = FakeRedis::start(Topology::Cluster, &[Behaviour::Healthy]).await;
            let replica = &redis.replicas[0].stats;
            let storage = connect_with(&redis, json!({ "pool_size": 2 })).await?;

            replica.stall();
            ping_replicas(&storage).await;
            replica.resume();

            // still left out until its period ends
            ping_replicas(&storage).await;
            let replica_reads = replica.reads();
            round_trip(&storage).await?;
            assert_eq!(
                replica.reads(),
                replica_reads,
                "the replica should be left out"
            );

            end_left_out_period(&storage);
            ping_replicas(&storage).await;
            assert_reads_go_only_to_replicas(&redis, &storage).await?;
            assert!(replica.reads() > replica_reads);
            Ok(())
        }

        /// A replica that couldn't serve commands when the pool connected gets reads once it
        /// recovers.
        #[tokio::test]
        async fn replica_unhealthy_at_startup_gets_reads_once_it_recovers() -> Result<(), BoxError>
        {
            let redis = FakeRedis::start(Topology::Cluster, &[Behaviour::Healthy]).await;
            let replica = &redis.replicas[0].stats;
            replica.stall();
            let storage = connect_with(&redis, json!({ "pool_size": 2 })).await?;
            round_trip(&storage).await?;
            assert_eq!(replica.reads(), 0, "the replica should be left out");

            replica.resume();
            end_left_out_period(&storage);
            ping_replicas(&storage).await;
            assert_reads_go_only_to_replicas(&redis, &storage).await?;
            assert!(replica.reads() > 0);
            Ok(())
        }

        /// Runs enough round trips to use every client in the pool, and checks none of them
        /// read from the primary.
        async fn assert_reads_go_only_to_replicas(
            redis: &FakeRedis,
            storage: &RedisCacheStorage,
        ) -> Result<(), BoxError> {
            let primary_reads = redis.primary.reads();
            for _ in 0..storage.redis_client_config.pool_size {
                round_trip(storage).await?;
            }
            assert_eq!(
                redis.primary.reads(),
                primary_reads,
                "every client should read from the replicas"
            );
            assert_pool_kept(storage);
            Ok(())
        }

        /// A cluster replica that restarts after the pool connected (closing its connections and
        /// refusing to serve new ones) doesn't fail the client.
        #[tokio::test]
        async fn replica_that_restarts_does_not_fail_the_pool() -> Result<(), BoxError> {
            let redis = FakeRedis::start(Topology::Cluster, &[Behaviour::Healthy]).await;
            let replica = &redis.replicas[0].stats;
            let storage = connect(&redis).await?;
            round_trip(&storage).await?;

            replica.restart();
            ping_replicas(&storage).await;

            let primary_reads = redis.primary.reads();
            round_trip(&storage).await?;
            assert!(
                redis.primary.reads() > primary_reads,
                "reads should fall back to the primary"
            );
            assert_pool_kept(&storage);
            Ok(())
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
        use std::sync::OnceLock;
        use std::sync::atomic::Ordering;

        use fred::interfaces::ClientLike;
        use fred::types::cluster::ClusterRouting;
        use itertools::Itertools;
        use rand::Rng as _;
        use rand::RngExt as _;
        use rand::distr::Alphanumeric;
        use serde_json::json;
        use tokio::sync::Mutex;
        use tower::BoxError;
        use uuid::Uuid;

        use crate::cache::redis::ACTIVE_CLIENT_COUNT;
        use crate::cache::redis::RedisCacheStorage;
        use crate::cache::redis::RedisKey;
        use crate::cache::redis::RedisValue;

        /// Serializes tests that create/drop Redis clients so that concurrent mutations
        /// to any global statics (like `ACTIVE_CLIENT_COUNT`) don't cause assertion failures
        ///
        /// NOTE: add this fn to your tests and take out a lock to force the tests to run serially;
        /// we must do this while ACTIVE_CLIENT_COUNT is a static because the test that checks
        /// whether its 'math' is correct becomes inderministic if the tests run in parallel (they
        /// affect its count)
        fn lock_for_static() -> &'static Mutex<()> {
            static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            LOCK.get_or_init(|| Mutex::new(()))
        }

        fn random_namespace() -> String {
            Uuid::new_v4().to_string()
        }

        /// The Redis deployments started by `docker-compose.yml` (and the CI test executor).
        #[derive(Clone, Copy, Debug)]
        enum Deployment {
            Standalone,
            /// A primary with a replica, which the client discovers with `ROLE`.
            PrimaryWithReplica,
            /// A sentinel monitoring the `PrimaryWithReplica` primary.
            Sentinel,
            Cluster,
        }
        use Deployment::*;

        impl Deployment {
            fn url(self) -> &'static str {
                match self {
                    Standalone => "redis://localhost:6379",
                    PrimaryWithReplica => "redis://localhost:6380",
                    Sentinel => {
                        "redis-sentinel://localhost:26379?sentinelServiceName=redis-primary"
                    }
                    Cluster => "redis-cluster://localhost:7000",
                }
            }

            fn is_cluster(self) -> bool {
                matches!(self, Cluster)
            }
        }

        fn redis_config(deployment: Deployment) -> crate::configuration::RedisCache {
            let config_json = json!({
                "urls": [deployment.url()],
                "namespace": random_namespace(),
                "required_to_start": true,
                "ttl": "60s"
            });

            serde_json::from_value(config_json).expect("invalid redis cache configuration")
        }

        async fn wait_for_recreation(storage: &RedisCacheStorage) {
            for _ in 0..100 {
                if storage.inner.read().is_some() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            panic!("timed out waiting for background recreation to complete");
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn new_starts_empty_and_create_client_pool_populates(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            assert!(storage.inner.read().is_none());

            storage.create_client_pool().await?;
            assert!(storage.inner.read().is_some());
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn accessor_recreates_when_inner_is_none(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
            #[values("client", "pipeline")] method: &str,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;
            storage.inner.write().take();
            assert!(storage.inner.read().is_none());

            let first_call_failed = match method {
                "client" => storage.client().await.is_err(),
                "pipeline" => storage.pipeline().await.is_err(),
                _ => unreachable!(),
            };

            assert!(first_call_failed, "expected error during recreation");
            wait_for_recreation(&storage).await;

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
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;

            let key = RedisKey("recreation_test_key".to_string());
            storage
                .insert(key.clone(), RedisValue("before".to_string()), None)
                .await;

            storage.inner.write().take();
            let _ = storage.client().await;
            wait_for_recreation(&storage).await;

            storage
                .insert(key.clone(), RedisValue("after".to_string()), None)
                .await;
            let fetched: RedisValue<String> = storage.get(key).await?;
            assert_eq!(fetched.0, "after");
            Ok(())
        }

        /// Recreation via client() or pipeline() should stop old tasks and activate new ones
        #[tokio::test]
        #[rstest::rstest]
        async fn recreation_stops_old_tasks_and_starts_new_ones(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
            #[values("client", "pipeline")] method: &str,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;

            let (old_heartbeat, old_metrics) = {
                let guard = storage.inner.read();
                let inner = guard.as_ref().unwrap();
                (
                    inner.heartbeat_abort_handle.clone(),
                    inner._metrics_collector.abort_handle(),
                )
            };
            assert!(!old_heartbeat.is_finished());
            assert!(!old_metrics.is_finished());

            storage.inner.write().take();
            tokio::task::yield_now().await;
            assert!(old_heartbeat.is_finished());
            assert!(old_metrics.is_finished());

            match method {
                "client" => {
                    let _ = storage.client().await;
                }
                "pipeline" => {
                    let _ = storage.pipeline().await;
                }
                _ => unreachable!(),
            }
            wait_for_recreation(&storage).await;

            let guard = storage.inner.read();
            let new_inner = guard.as_ref().unwrap();
            assert!(!new_inner.heartbeat_abort_handle.is_finished());
            let new_metrics = new_inner._metrics_collector.abort_handle();
            assert!(!new_metrics.is_finished());
            Ok(())
        }

        /// The replica keep-alive heartbeat should run only for clustered clients (which
        /// establish replica connections) and be absent for standalone clients.
        #[tokio::test]
        #[rstest::rstest]
        async fn replica_heartbeat_present_only_when_clustered(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;

            let guard = storage.inner.read();
            let inner = guard.as_ref().unwrap();
            if deployment.is_cluster() {
                let handle = inner
                    .replica_heartbeat_abort_handle
                    .as_ref()
                    .expect("clustered pool should run a replica heartbeat");
                assert!(!handle.is_finished());
            } else {
                assert!(
                    inner.replica_heartbeat_abort_handle.is_none(),
                    "standalone pool should not run a replica heartbeat"
                );
            }
            Ok(())
        }

        /// After a pool recreation, a clustered pool should have a fresh, live replica heartbeat
        /// and the old one should have been aborted along with the old pool.
        #[tokio::test]
        async fn recreation_restarts_replica_heartbeat() -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(Cluster), "test").await?;
            storage.create_client_pool().await?;

            let old_replica_heartbeat = storage
                .inner
                .read()
                .as_ref()
                .unwrap()
                .replica_heartbeat_abort_handle
                .clone()
                .expect("clustered pool should run a replica heartbeat");
            assert!(!old_replica_heartbeat.is_finished());

            storage.inner.write().take();
            tokio::task::yield_now().await;
            assert!(
                old_replica_heartbeat.is_finished(),
                "old replica heartbeat should be aborted when the pool is dropped"
            );

            let _ = storage.client().await;
            wait_for_recreation(&storage).await;

            let guard = storage.inner.read();
            let new_replica_heartbeat = guard
                .as_ref()
                .unwrap()
                .replica_heartbeat_abort_handle
                .as_ref()
                .expect("recreated clustered pool should run a replica heartbeat");
            assert!(!new_replica_heartbeat.is_finished());
            Ok(())
        }

        /// create_client_pool() starts the metrics collection task
        #[tokio::test]
        #[rstest::rstest]
        async fn create_client_pool_starts_metrics(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;

            // before create_client_pool() there is no pool and no metrics collector
            assert!(storage.inner.read().as_ref().is_none());

            storage.create_client_pool().await?;

            let handle = storage
                .inner
                .read()
                .as_ref()
                .unwrap()
                ._metrics_collector
                .abort_handle();
            assert!(!handle.is_finished());
            Ok(())
        }

        /// Pipeline operations (batched commands) should work after client recreation.
        /// insert_multiple uses pipeline() under the hood.
        #[tokio::test]
        #[rstest::rstest]
        async fn pipeline_operations_work_after_recreation(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;

            storage.inner.write().take();
            let _ = storage.client().await;
            wait_for_recreation(&storage).await;

            let data = vec![
                (
                    RedisKey("pipe_key_1".to_string()),
                    RedisValue("val_1".to_string()),
                ),
                (
                    RedisKey("pipe_key_2".to_string()),
                    RedisValue("val_2".to_string()),
                ),
            ];
            storage.insert_multiple(&data, None).await?;

            let fetched1: RedisValue<String> =
                storage.get(RedisKey("pipe_key_1".to_string())).await?;
            let fetched2: RedisValue<String> =
                storage.get(RedisKey("pipe_key_2".to_string())).await?;
            assert_eq!(fetched1.0, "val_1");
            assert_eq!(fetched2.0, "val_2");
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn clones_share_recreated_client(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;
            let clone = storage.clone();

            storage.inner.write().take();
            assert!(clone.inner.read().is_none(), "clone should also see None");

            let _ = storage.client().await;
            wait_for_recreation(&storage).await;
            assert!(
                clone.inner.read().is_some(),
                "clone should see the recreated client"
            );
            assert!(clone.client().await.is_ok());
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        #[rstest::rstest]
        async fn concurrent_recreation_does_not_panic_or_deadlock(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;
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
            wait_for_recreation(&storage).await;
            assert!(storage.client().await.is_ok());
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        #[rstest::rstest]
        async fn recreation_lock_held_during_spawned_task(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;
            storage.inner.write().take();

            let _ = storage.client().await;
            assert!(
                storage
                    .pool_recreation_lock
                    .clone()
                    .try_lock_owned()
                    .is_err(),
                "lock should be held by the spawned recreation task"
            );

            wait_for_recreation(&storage).await;
            assert!(
                storage
                    .pool_recreation_lock
                    .clone()
                    .try_lock_owned()
                    .is_ok(),
                "lock should be released after recreation completes"
            );
            Ok(())
        }

        /// scan_with_namespaced_results should work after client recreation
        #[tokio::test]
        #[rstest::rstest]
        async fn scan_works_after_recreation(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            use fred::types::scan::Scanner;
            use futures::StreamExt;

            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;
            let key = RedisKey("scan_test_key".to_string());
            storage
                .insert(key, RedisValue("value".to_string()), None)
                .await;

            storage.inner.write().take();
            let _ = storage.client().await;
            wait_for_recreation(&storage).await;

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
            let _guard = lock_for_static().lock().await;
            let storage =
                RedisCacheStorage::new(redis_config(Cluster), "test_redis_storage").await?;
            storage.create_client_pool().await?;

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

        /// Calling create_client_pool() on an already-connected storage replaces the pool. The old
        /// pool's watcher must not clear the new pool from `inner`.
        #[tokio::test]
        #[rstest::rstest]
        async fn create_client_pool_replaces_existing_inner(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;

            let (old_heartbeat, old_watcher) = {
                let guard = storage.inner.read();
                let inner = guard.as_ref().unwrap();
                (
                    inner.heartbeat_abort_handle.clone(),
                    inner.watcher_abort_handle.clone(),
                )
            };
            assert!(!old_heartbeat.is_finished());
            assert!(!old_watcher.is_finished());

            storage.create_client_pool().await?;
            tokio::task::yield_now().await;

            assert!(old_heartbeat.is_finished());
            assert!(old_watcher.is_finished());
            assert!(
                storage.inner.read().is_some(),
                "new pool should not have been cleared by old watcher"
            );
            assert!(storage.client().await.is_ok());
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn independent_storages_simulate_config_reload(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let old_storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            old_storage.create_client_pool().await?;
            let (old_heartbeat, old_watcher) = {
                let guard = old_storage.inner.read();
                let inner = guard.as_ref().unwrap();
                (
                    inner.heartbeat_abort_handle.clone(),
                    inner.watcher_abort_handle.clone(),
                )
            };

            let new_storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            new_storage.create_client_pool().await?;

            // drop the old storage, simulating the router swapping to a new config
            drop(old_storage);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // old pool's tasks should be cleaned up (no leak)
            assert!(old_heartbeat.is_finished());
            assert!(old_watcher.is_finished());

            // new storage is unaffected and can still operate
            assert!(new_storage.inner.read().is_some());
            assert!(new_storage.client().await.is_ok());
            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn pool_shutdown_marks_inner_for_recreation(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;
            assert!(storage.inner.read().is_some());

            let pool_clone = {
                let guard = storage.inner.read();
                guard.as_ref().unwrap().pool.clone()
            };
            pool_clone.quit().await?;

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            assert!(
                storage.inner.read().is_none(),
                "inner should be None after pool shutdown"
            );

            let result = storage.client().await;

            assert!(result.is_err(), "first call after abort should error");
            wait_for_recreation(&storage).await;
            assert!(storage.client().await.is_ok());
            Ok(())
        }

        /// Test that `get_multiple` returns items in the correct order.
        #[tokio::test]
        #[rstest::rstest]
        async fn test_get_multiple_is_ordered(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let storage =
                RedisCacheStorage::new(redis_config(deployment), "test_get_multiple_is_ordered")
                    .await?;
            storage.create_client_pool().await?;

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

        #[tokio::test]
        #[rstest::rstest]
        async fn active_client_count_is_balanced_after_drop(
            #[values(Standalone, PrimaryWithReplica, Sentinel, Cluster)] deployment: Deployment,
        ) -> Result<(), BoxError> {
            let _guard = lock_for_static().lock().await;
            let before = ACTIVE_CLIENT_COUNT.load(Ordering::Relaxed);

            let storage = RedisCacheStorage::new(redis_config(deployment), "test").await?;
            storage.create_client_pool().await?;
            let after_create = ACTIVE_CLIENT_COUNT.load(Ordering::Relaxed);

            assert!(
                after_create > before,
                "count should increase after creating client"
            );

            drop(storage);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            let after_drop = ACTIVE_CLIENT_COUNT.load(Ordering::Relaxed);
            assert_eq!(
                after_drop, before,
                "count should return to original after drop"
            );
            Ok(())
        }
    }
}
