use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fred::clients::Client;
use fred::clients::Pipeline;
use fred::interfaces::EventInterface;
use fred::interfaces::SortedSetsInterface;
#[cfg(test)]
use fred::mocks::Mocks;
use fred::prelude::ClientLike;
use fred::prelude::HeartbeatInterface;
use fred::prelude::KeysInterface;
use fred::prelude::Options;
use fred::prelude::TcpConfig;
use fred::types::Builder;
use fred::types::Expiration;
use fred::types::config::ClusterDiscoveryPolicy;
use fred::types::config::Config as RedisConfig;
use fred::types::config::ReconnectPolicy;
use fred::types::config::TlsConfig;
use fred::types::config::TlsHostMapping;
use fred::types::config::UnresponsiveConfig;
use fred::types::scan::ScanResult;
use fred::types::scan::Scanner;
use futures::Stream;
use futures::TryStreamExt;
use futures::future::join_all;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;
use tokio_stream::StreamExt;
use tower::BoxError;
use url::Url;

use super::Error;
use super::KeyType;
use super::ValueType;
use super::error::record as record_redis_error;
use super::key::Key;
use super::metrics::RedisMetricsCollector;
use super::pool::DropSafeRedisPool;
use super::value::Value;
use crate::configuration::RedisCache;
use crate::services::generate_tls_client_config;

// TODO document this:
//  note that there aren't tokio::timeouts happening within the gateway, other than the client timeouts
//  configured in the router config.

pub(crate) static ACTIVE_CLIENT_COUNT: AtomicU64 = AtomicU64::new(0);

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

#[derive(Clone)]
pub(crate) struct Gateway {
    inner: Arc<DropSafeRedisPool>,
    namespace: Option<Arc<String>>,
    pub(crate) ttl: Option<Duration>,
    is_cluster: bool,
    reset_ttl: bool,
}

impl Gateway {
    pub(crate) async fn new(config: RedisCache, caller: &'static str) -> Result<Self, BoxError> {
        let url = Self::preprocess_urls(config.urls)
            .inspect_err(|err| record_redis_error(err, caller, "startup"))?;
        let mut client_config = RedisConfig::from_url(url.as_str())
            .map_err(Into::into)
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

        Self::create_client(
            client_config,
            config.timeout,
            config.pool_size as usize,
            config.namespace,
            config.ttl,
            config.reset_ttl,
            is_cluster,
            caller,
            config.metrics_interval,
            config.required_to_start,
        )
        .await
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

        Self::create_client(
            client_config,
            config.timeout,
            config.pool_size as usize,
            config.namespace,
            config.ttl,
            config.reset_ttl,
            is_cluster,
            caller,
            config.metrics_interval,
            true,
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
        required_to_start: bool,
    ) -> Result<Self, BoxError> {
        let pooled_client = Builder::from_config(client_config)
            .with_config(|client_config| {
                if is_cluster {
                    // use `ClusterDiscoveryPolicy::ConfigEndpoint` - explicit in case the default changes.
                    // this determines how the clients discover other cluster nodes
                    let _ = client_config
                        .server
                        .set_cluster_discovery_policy(ClusterDiscoveryPolicy::ConfigEndpoint)
                        .map_err(Error::from)
                        .inspect_err(|err| record_redis_error(err, caller, "startup"));
                }
            })
            .with_connection_config(|config| {
                config.internal_command_timeout = DEFAULT_INTERNAL_REDIS_TIMEOUT;
                config.max_command_buffer_len = 10_000;
                config.reconnect_on_auth_error = true;
                config.tcp = TcpConfig {
                    #[cfg(target_os = "linux")]
                    user_timeout: Some(timeout),
                    ..Default::default()
                };
                config.unresponsive = UnresponsiveConfig {
                    max_timeout: Some(DEFAULT_INTERNAL_REDIS_TIMEOUT),
                    interval: Duration::from_secs(3),
                };

                // PR-8405: must not use lazy connections or else commands will queue rather than being sent
                // PR-8671: must only disable lazy connections in cluster mode. otherwise, fred will
                //  try to connect to unreachable replicas and fall over.
                //  https://github.com/aembke/fred.rs/blob/f222ad7bfba844dbdc57e93da61b0a5483858df9/src/router/replicas.rs#L34
                if is_cluster {
                    config.replica.lazy_connections = false;
                }
            })
            .with_performance_config(|config| {
                config.default_command_timeout = timeout;
            })
            .set_policy(ReconnectPolicy::new_exponential(0, 1, 2000, 5))
            .build_pool(pool_size)?;

        for client in pooled_client.clients() {
            // spawn tasks that listen for connection close or reconnect events
            let mut error_rx = client.error_rx();
            let mut reconnect_rx = client.reconnect_rx();
            let mut unresponsive_rx = client.unresponsive_rx();

            ACTIVE_CLIENT_COUNT.fetch_add(1, Ordering::Relaxed);

            tokio::spawn(async move {
                loop {
                    match error_rx.recv().await {
                        Ok((error, _)) => record_redis_error(&error, caller, "client"),
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    }
                }
            });

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

        // NB: error is not recorded here as it will be observed by the task following `client.error_rx()`
        let client_handles = pooled_client.connect_pool();
        if required_to_start {
            pooled_client.wait_for_connect().await?;
            tracing::trace!("redis connections established");
        }

        tokio::spawn(async move {
            // the handles will resolve when the clients finish terminating. per the `fred` docs:
            // > [the connect] function returns a `JoinHandle` to a task that drives the connection.
            // > It will not resolve until the connection closes, or if a reconnection policy with
            // > unlimited attempts is provided then it will run until `QUIT` is called.
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
        let metrics_collector =
            RedisMetricsCollector::new(pooled_client_arc.clone(), caller, metrics_interval);

        Ok(Self {
            inner: Arc::new(DropSafeRedisPool {
                pool: pooled_client_arc,
                caller,
                heartbeat_abort_handle: heartbeat_handle.abort_handle(),
                _metrics_collector: metrics_collector,
            }),
            namespace: namespace.map(Arc::new),
            ttl,
            is_cluster,
            reset_ttl,
        })
    }

    /// Helper method to record Redis errors for metrics
    fn record_query_error<E: Into<Error>>(&self, error: &RedisError) {
        record_redis_error(error.into(), self.inner.caller, "query");
    }

    fn preprocess_urls(urls: Vec<Url>) -> Result<Url, Error> {
        let url_len = urls.len();
        let mut urls_iter = urls.into_iter();
        let first = urls_iter.next();
        match first {
            None => Err(Error::Configuration("Empty Redis URL list".to_string())),
            Some(first) => {
                let scheme = first.scheme();
                if !SUPPORTED_REDIS_SCHEMES.contains(&scheme) {
                    return Err(Error::Configuration(format!(
                        "Invalid Redis URL scheme, expected a scheme from {SUPPORTED_REDIS_SCHEMES:?}, got: {scheme}"
                    )));
                }

                if url_len == 1 {
                    return Ok(first.clone());
                }

                let username = first.username();
                let password = first.password();

                let mut result = first.clone();
                for mut url in urls_iter {
                    if url.username() != username {
                        return Err(Error::Configuration(
                            "Incompatible usernames between Redis URLs".to_string(),
                        ));
                    }
                    if url.password() != password {
                        return Err(Error::Configuration(
                            "Incompatible passwords between Redis URLs".to_string(),
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
                        return Err(Error::Configuration(
                            "Incompatible schemes between Redis URLs".to_string(),
                        ));
                    }

                    let host = url.host_str().ok_or_else(|| {
                        Error::Configuration("Missing host in Redis URL".to_string())
                    })?;

                    let port = url.port().ok_or_else(|| {
                        Error::Configuration("Missing port in Redis URL".to_string())
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

    pub(crate) fn client(&self) -> Client {
        self.inner.next().clone()
    }

    pub(crate) fn pipeline(&self) -> Pipeline<Client> {
        self.inner.next().pipeline()
    }

    fn expiration(&self, ttl: Option<Duration>) -> Option<Expiration> {
        let ttl = ttl.or(self.ttl)?;
        Some(Expiration::EX(ttl.as_secs() as i64))
    }

    pub(crate) fn make_key<K: KeyType>(&self, key: K) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}:{key}"),
            None => key.to_string(),
        }
    }

    pub(crate) async fn get_multiple<K: KeyType, V: ValueType>(
        &self,
        keys: Vec<K>,
    ) -> Result<Vec<Result<Option<V>, Error>>, Error> {
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
        keys: Vec<K>,
        options: Options,
    ) -> Result<Vec<Result<Option<V>, Error>>, Error> {
        // NB: MGET is different from GET in that it returns `Option`s rather than `Result`s.
        //  > For every key that does not hold a string value or does not exist, the special value
        //    nil is returned. Because of this, the operation never fails.
        //    - https://redis.io/docs/latest/commands/mget/

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
            let client = self.client().replicas().with_options(&options);
            let mut tasks = Vec::with_capacity(len);
            for (index, key) in keys.into_iter().enumerate() {
                let client = client.clone();
                tasks.push(async move {
                    let res_value: Result<Value<Option<V>>, _> =
                        client.get(self.make_key(key)).await;
                    (index, res_value)
                })
            }

            let mut results_with_indexes = join_all(tasks).await;
            results_with_indexes.sort_unstable_by_key(|(index, _)| *index);
            Ok(results_with_indexes
                .into_iter()
                .map(|(_, value)| {
                    value
                        .inspect_err(|e| self.record_error(e))
                        .map_err(Into::into)
                        .map(|res| res.0)
                })
                .collect())
        } else {
            let keys = keys
                .into_iter()
                .map(|k| self.make_key(k))
                .collect::<Vec<_>>();
            let values: Vec<Value<Option<V>>> = self
                .client()
                .with_options(&options)
                .mget(keys)
                .await
                .inspect_err(|e| self.record_error(e))?;
            Ok(values.into_iter().map(|v| Ok(v.0)).collect())
        }
    }

    pub(crate) async fn insert<K: KeyType, V: ValueType>(
        &self,
        key: K,
        value: V,
        ttl: Option<Duration>,
    ) {
        let key = self.make_key(key);
        tracing::trace!("inserting into redis: {:?}, {:?}", key, value);

        // NOTE: we need a writer, so don't use replicas() here
        let result: Result<(), _> = self
            .client()
            .set(
                Key::from(key),
                Value::from(value),
                self.expiration(ttl),
                None,
                false,
            )
            .await;
        tracing::trace!("insert result {:?}", result);

        if let Err(err) = result {
            self.record_query_error(&err);
        }
    }

    pub(crate) async fn insert_multiple<K: KeyType, V: ValueType>(
        &self,
        data: &[(K, V)],
        ttl: Option<Duration>,
    ) {
        tracing::trace!("inserting into redis: {:#?}", data);
        let expiration = self.expiration(ttl);

        // NB: if we were using MSET here, we'd need to split the keys by hash slot. however, fred
        // seems to split the pipeline by hash slot in the background.
        let pipeline = self.pipeline();
        for (key, value) in data {
            let key = self.make_key(key.clone());
            let _: Result<(), _> = pipeline
                .set(
                    key,
                    Value::from(value.clone()),
                    expiration.clone(),
                    None,
                    false,
                )
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
    }
}

// Public API for Redis commands - function names should be strongly mapped to redis command names.
impl Gateway {
    pub(crate) async fn del<K: KeyType, I: Iterator<Item = K>>(
        &self,
        keys: I,
    ) -> Result<u64, Error> {
        let keys: Vec<String> = keys.into_iter().map(|k| self.make_key(k)).collect();
        self.delete_keys(keys).await
    }

    pub(crate) async fn expireat<K: KeyType>(
        &self,
        key: K,
        timestamp: i64,
        options: Option<super::options::expire::Options>,
    ) -> Result<(), Error> {
        let key = self.make_key(key);
        let _: Value<Option<usize>> = self
            .client()
            .expire_at(key, timestamp, options.map(Into::into))
            .await?;
        Ok(())
    }

    pub(crate) async fn get<K: KeyType, V: ValueType>(&self, key: K) -> Result<Option<V>, Error> {
        let key = self.make_key(key);

        let result: Result<Value<Option<V>>, _> = if self.is_cluster {
            self.client().replicas().get(key).await
        } else {
            self.client().get(key).await
        };

        let value = result.inspect_err(|e| self.record_error(e))?;
        Ok(value.0)
    }

    // get key _and_ update TTL IFF self.reset_ttl && self.ttl.is_some
    // otherwise, fall back to just get
    pub(crate) async fn getex<K: KeyType, V: ValueType>(&self, key: K) -> Result<Option<V>, Error> {
        // NB: GETEX was released in redis OSS 6.2 (feb 2022) but fred doesn't support it for some reason.
        //  use a pipeline instead
        if self.reset_ttl
            && let Some(ttl) = self.ttl
        {
            let key = self.make_key(key);
            let pipeline = self.pipeline();
            let _: () = pipeline
                .get(&key)
                .await
                .inspect_err(|e| self.record_error(e))?;
            let _: () = pipeline
                .expire(&key, ttl.as_secs() as i64, None)
                .await
                .inspect_err(|e| self.record_error(e))?;

            let (value, _timeout_set): (Value<Option<V>>, bool) =
                pipeline.all().await.inspect_err(|e| self.record_error(e))?;
            Ok(value.0)
        } else {
            self.get(key).await
        }
    }

    fn scan<K: KeyType>(
        &self,
        pattern: K,
        count: Option<u32>,
    ) -> Pin<Box<dyn Stream<Item = Result<ScanResult, Error>> + Send>> {
        let pattern = self.make_key(pattern);
        if self.is_cluster {
            // NOTE: scans might be better send to only the read replicas, but the read-only client
            // doesn't have a scan_cluster(), just a paginated version called scan_page()
            Box::pin(
                self.client()
                    .scan_cluster(pattern, count, None)
                    .map_err(Error::from),
            )
        } else {
            Box::pin(
                self.client()
                    .scan(pattern, count, None)
                    .map_err(Error::from),
            )
        }
    }

    // TODO: merge with insert
    pub(crate) async fn set<K: KeyType, V: ValueType>(
        &self,
        key: K,
        value: V,
        expiration: Option<super::options::Expiration>,
    ) -> Result<(), Error> {
        let key = self.make_key(key);
        let _: Value<Option<()>> = self
            .client()
            .set(key, Value(value), expiration.map(Into::into), None, false)
            .await?;
        Ok(())
    }

    pub(crate) async fn zadd<K: KeyType, V: ValueType>(
        &self,
        key: K,
        elements: Vec<(f64, V)>,
        ordering: Option<super::options::zadd::Ordering>,
    ) -> Result<(), Error> {
        let key = self.make_key(key);
        let elements: Vec<(f64, fred::types::Value)> = elements
            .into_iter()
            .map(|(score, element)| {
                let parsed_value: Result<fred::types::Value, _> = Value(element).try_into();
                parsed_value.map(|v| (score, v))
            })
            .collect::<Result<Vec<(_, _)>, _>>()?;

        let _added: u64 = self
            .client()
            .zadd(key, None, ordering.map(Into::into), false, false, elements)
            .await?;
        Ok(())
    }

    pub(crate) async fn zrange<K: KeyType, V: ValueType>(
        &self,
        key: K,
        min_index: i64,
        max_index: i64,
    ) -> Result<Vec<V>, Error> {
        let key = self.make_key(key);
        // NB: Option<> shouldn't be possible, but it makes the types easier when working with fred
        // TODO: document this better
        let elements: Vec<Value<Option<V>>> = self
            .client()
            .zrange(key, min_index, max_index, None, false, None, false)
            .await?;
        Ok(elements.into_iter().filter_map(|v| v.0).collect())
    }

    pub(crate) async fn zremrangebyscore<K: KeyType>(
        &self,
        key: K,
        min_score: f64,
        max_score: f64,
    ) -> Result<u64, Error> {
        let key = self.make_key(key);
        let items_removed = self
            .client()
            .zremrangebyscore(key, min_score, max_score)
            .await?;
        Ok(items_removed)
    }
}

// Public API for Redis commands used in testing - function names should be strongly mapped to redis command names.
#[cfg(test)]
impl Gateway {
    pub(crate) async fn expiretime<K: KeyType>(&self, key: K) -> Result<Option<i64>, Error> {
        let key = self.make_key(key);
        let value = self.client().expire_time(key).await?;

        // * value == -1 if the key exists but has no expire time
        // * value == -2 if the key does not exist
        if value >= 0 {
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn ttl<K: KeyType>(&self, key: K) -> Result<Option<Duration>, Error> {
        let key = self.make_key(key);
        let value: i64 = self.client().ttl(key).await?;

        // * value == -1 if the key exists but has no expire time
        // * value == -2 if the key does not exist
        if value >= 0 {
            Ok(Some(Duration::new(value as u64, 0)))
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn zcard<K: KeyType>(&self, key: K) -> Result<Option<u64>, Error> {
        let key = self.make_key(key);
        let value = self.client().zcard(key).await?;
        Ok(value)
    }

    pub(crate) async fn zscore<K: KeyType, L: KeyType>(
        &self,
        key: K,
        member: L,
    ) -> Result<Option<f64>, Error> {
        let key = self.make_key(key);
        let member = member.to_string();
        let value = self.client().zscore(key, member).await?;
        Ok(value)
    }
}

// Public API for abstractions on top of redis commands
impl Gateway {
    // delete all elements matching {namespace}:{pattern}
    // NB: weird return type to expose the fact that we can be partially through a delete operation
    // before encountering an error. TODO: is this actulaly necessary?
    pub(crate) async fn scan_and_delete<K: KeyType>(
        &self,
        pattern: K,
        count: Option<u32>,
    ) -> Result<u64, (u64, Error)> {
        let mut deleted = 0;
        let mut scan_result_stream = self.scan(pattern, count);
        while let Some(scan_result) = scan_result_stream.next().await {
            let mut scan_result: ScanResult = scan_result.map_err(|err| (deleted, err.into()))?;
            if let Some(keys) = scan_result.take_results()
                && !keys.is_empty()
            {
                // turn keys into Vec<String> and pass to the delete function
                // NOTE: these keys are already namespaced, which is why we use self.delete_keys
                // rather than self.del
                let keys = keys
                    .into_iter()
                    .filter_map(|key| key.into_string())
                    .collect();
                match self.delete_keys(keys).await {
                    Ok(count) => deleted += count,
                    Err(err) => return Err((deleted, err.into())),
                }
            }
        }

        Ok(deleted)
    }
}

// Public API for abstractions on top of redis commands
#[cfg(test)]
impl Gateway {
    // TODO: real fn accepts a list of keys and returns the number of those that exist... but that's not
    //  needed at this time
    pub(crate) async fn exists<K: KeyType>(&self, key: K) -> Result<bool, Error> {
        let key = self.make_key(key);
        let count: u64 = self.client().exists(key).await?;
        Ok(count == 1)
    }

    pub(crate) async fn truncate_namespace(&self) -> Result<(), Error> {
        if self.namespace.is_none() {
            panic!("Will not truncate as no namespace was provided");
        }
        match self.scan_and_delete("*", Some(100)).await {
            Ok(_) => Ok(()),
            Err((_, err)) => Err(err),
        }
    }

    /// TODO better docs Return a list of all keys in this namespace, with the namespace string stripped from
    /// each key.
    pub(crate) async fn all_keys_in_namespace(&self) -> Result<Vec<String>, Error> {
        let mut keys = Vec::new();
        let mut scan_result_stream = self.scan("*", None);
        while let Some(scan_result) = scan_result_stream.next().await {
            if let Some(page_keys) = scan_result?.take_results() {
                keys.extend(
                    page_keys
                        .into_iter()
                        .filter_map(|key| key.into_string())
                        .map(|key| self.strip_namespace(key)),
                );
            }
        }

        Ok(keys)
    }

    // see if a sorted set member exists. returns Ok(false) if (a) the sorted set doesn't exist
    // or (b) the member doesn't exist in the set
    pub(crate) async fn zexists<K: KeyType, L: KeyType>(
        &self,
        key: K,
        member: L,
    ) -> Result<bool, Error> {
        Ok(self.zscore(key, member).await?.is_some())
    }
}

#[cfg(test)]
// Other misc stuff
impl Gateway {
    fn strip_namespace(&self, key: String) -> String {
        match &self.namespace {
            Some(namespace) => key
                .strip_prefix(&format!("{namespace}:"))
                .map(ToString::to_string)
                .unwrap_or(key),
            None => key,
        }
    }
}

// Other misc stuff that doesn't map directly to a key
impl Gateway {
    // TODO doc: this doesn't postprocess the keys to add the namespace
    async fn delete_keys(&self, keys: Vec<String>) -> Result<u64, Error> {
        if self.is_cluster {
            // execute each DEL in a joinset
            let mut join_set: JoinSet<Result<u64, _>> = JoinSet::new();
            for key in keys {
                let client = self.client();
                join_set.spawn(async move { client.del(key).await });
            }

            let mut count = 0;
            while let Some(result) = join_set.join_next().await {
                count += result??;
            }
            Ok(count)
        } else {
            let count = self.client().del(keys).await?;
            Ok(count)
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::SystemTime;

    use url::Url;

    use crate::redis;

    #[test]
    fn ensure_invalid_payload_serialization_doesnt_fail() {
        #[derive(
            Clone, Debug, serde::Serialize, serde::Deserialize, redis_derive::SerializableValue,
        )]
        struct Stuff {
            time: SystemTime,
        }

        let invalid_json_payload = redis::value::Value(Stuff {
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

        let preprocess_result = redis::Gateway::preprocess_urls(urls);
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

        let preprocess_result = redis::Gateway::preprocess_urls(urls);
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
            let preprocess_result = redis::Gateway::preprocess_urls(url_pairing);
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
            let preprocess_result = redis::Gateway::preprocess_urls(url_pairing.clone());
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
        use rand::Rng;
        use rand::RngCore;
        use rand::distr::Alphanumeric;
        use serde_json::json;
        use tower::BoxError;
        use uuid::Uuid;

        use crate::redis;

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

        /// Tests that `insert_multiple` and `get_multiple` are successful when run against clustered Redis.
        ///
        /// Clustered Redis works by hashing each key to one of 16384 hash slots, and assigning each hash
        /// slot to a node. Operations which interact with multiple keys (`MGET`, `MSET`) *cannot* be
        /// used on keys which map to different hash slots, even if those hash slots are on the same node.
        ///
        /// This test inserts data that is guaranteed to hash to different slots to verify that
        /// `redis::Gateway` is well-behaved when operating against a cluster.
        #[tokio::test(flavor = "multi_thread")]
        async fn test_redis_storage_avoids_common_cross_slot_errors() -> Result<(), BoxError> {
            let clustered = true;
            let storage =
                redis::Gateway::new(redis_config(clustered), "test_redis_storage").await?;

            // insert values which reflect different cluster slots
            let mut data = HashMap::default();
            let expected_value = rand::rng().next_u32() as usize;
            let unique_cluster_slot_count = |data: &HashMap<String, _>| {
                data.keys()
                    .map(|key| ClusterRouting::hash_key(key.as_bytes()))
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
                data.insert(format!("{{{}}}", key), expected_value);
            }

            // insert values
            let keys: Vec<_> = data.keys().cloned().collect();
            let data: Vec<_> = data.into_iter().collect();
            storage.insert_multiple(&data, None).await;

            // make a `get` call for each key and ensure that it has the expected value. this tests both
            // the `get` and `insert_multiple` functions
            for key in &keys {
                let value: usize = storage.get(key.clone()).await?.expect("not found");
                assert_eq!(value, expected_value);
            }

            // test the `mget` functionality
            let values = storage.get_multiple(keys).await?;
            for value in values {
                let value: usize = value?.ok_or("missing value")?;
                assert_eq!(value, expected_value);
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
                redis::Gateway::new(redis_config(clustered), "test_get_multiple_is_ordered")
                    .await?;

            let data =
                [("a", "1"), ("b", "2"), ("c", "3")].map(|(k, v)| (k.to_string(), v.to_string()));
            storage.insert_multiple(&data, None).await;

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
                let expected_values: Vec<Option<String>> = expected_values
                    .into_iter()
                    .map(|value| value.map(ToString::to_string))
                    .collect();

                let values = storage.get_multiple(keys).await?;
                let parsed_values: Vec<Option<String>> =
                    values.into_iter().map(|v| v.ok().flatten()).collect();
                assert_eq!(parsed_values, expected_values);
            }

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn test_truncation_removes_all_keys_in_namespace(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let storage = redis::Gateway::new(
                redis_config(clustered),
                "test_truncation_removes_all_keys_in_namespace",
            )
            .await?;

            storage.truncate_namespace().await?;
            assert!(storage.all_keys_in_namespace().await?.is_empty());

            // add a few keys to this namespace
            let data = [("hello", "world".to_string()), ("foo", "bar".to_string())];
            storage.insert_multiple(&data, None).await;

            assert!(storage.exists("hello").await?);
            assert!(storage.exists("foo").await?);

            let keys = storage.all_keys_in_namespace().await?;
            assert!(!keys.is_empty());
            assert_eq!(keys.len(), 2);

            // truncate the namespace and make sure it's empty again
            storage.truncate_namespace().await?;
            assert!(storage.all_keys_in_namespace().await?.is_empty());

            Ok(())
        }
    }
}
