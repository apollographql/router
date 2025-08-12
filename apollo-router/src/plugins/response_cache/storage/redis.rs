use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

use fred::clients::Client;
use fred::clients::Pipeline;
use fred::interfaces::ConfigInterface;
use fred::interfaces::EventInterface;
use fred::interfaces::KeysInterface;
use fred::interfaces::LuaInterface;
use fred::interfaces::PubsubInterface;
use fred::interfaces::SortedSetsInterface;
use fred::types::Expiration;
use fred::types::Value;
use fred::types::scan::Scanner;
use fred::types::sorted_sets::Ordering;
use futures::StreamExt;
use futures::future::join_all;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::MissedTickBehavior;
use tower::BoxError;

use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::cache::storage::ValueType;
use crate::plugins::response_cache::cache_control::CacheControl;
use crate::plugins::response_cache::cache_control::now_epoch_seconds;
use crate::plugins::response_cache::storage::CacheEntry;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::Document;
use crate::plugins::response_cache::storage::Documents;
use crate::plugins::response_cache::storage::Error;
use crate::plugins::response_cache::storage::StorageResult;

pub(crate) type Config = crate::configuration::RedisCache;

#[derive(Deserialize, Debug, Clone, Serialize)]
struct CacheValue {
    data: serde_json_bytes::Value,
    cache_control: CacheControl,
}

impl ValueType for CacheValue {}

impl TryFrom<(&str, CacheValue)> for CacheEntry {
    type Error = serde_json::Error;

    fn try_from((cache_key, cache_value): (&str, CacheValue)) -> Result<Self, Self::Error> {
        Ok(CacheEntry {
            cache_key: cache_key.to_string(),
            data: cache_value.data,
            control: cache_value.cache_control,
        })
    }
}

#[derive(Clone)]
pub(crate) struct Storage {
    storage: RedisCacheStorage,
    keyspace_storage: RedisCacheStorage,
}

impl Storage {
    pub(crate) async fn new(config: &Config) -> Result<Self, BoxError> {
        // TODO: make this work for multiple storages
        let storage = RedisCacheStorage::new(config.clone(), "response-cache").await?;
        let keyspace_storage =
            RedisCacheStorage::new(config.clone(), "response-cache-keyspace").await?;

        let s = Storage {
            storage,
            keyspace_storage,
        };

        // TODO: also test performance, db size, etc without keyevents!
        s.enable_keyevent_notifications().await;
        s.subscribe_to_keyspace_events().await;

        s.perform_periodic_maintenance().await;

        Ok(s)
    }

    fn make_key<S: Into<String>>(&self, key: S) -> String {
        self.storage.make_key(RedisKey(key.into()))
    }

    fn primary_cache_key(key: &str) -> String {
        // surround key with curly braces so that the key determines the shard (if enabled)
        format!("pck:{{{key}}}")
    }

    fn cache_tags_key(key: &str) -> String {
        // surround key with curly braces so that the key determines the shard (if enabled)
        // this ensures that the primary cache key and its cache tags live on the same shard
        format!("cache-tags:{{{key}}}")
    }

    fn pck_prefix(&self) -> String {
        self.storage.make_key(RedisKey("pck:"))
    }

    async fn add_insert_to_pipeline(
        &self,
        pipeline: &Pipeline<Client>,
        mut document: Document,
        now: u64,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        // TODO: how does this work with multiple shards?
        let expire_at = now + document.expire.as_secs();
        let expire_at_doubled = now + document.expire.as_secs() * 2;

        let pck = self.make_key(Self::primary_cache_key(&document.cache_key));
        let value = CacheValue {
            data: document.data,
            cache_control: document.cache_control,
        };

        // TODO: figure out if this is actually how we want to store the values
        let _: () = pipeline
            .set(
                pck.clone(),
                &serde_json::to_string(&value).unwrap(),
                Some(Expiration::EXAT(expire_at as i64)),
                None,
                false,
            )
            .await?;

        // add 'subgraph' to invalidation keys
        // TODO: make sure we can't clobber things etc - pick different naming formats
        let keys = document.invalidation_keys.clone();
        for key in keys {
            document
                .invalidation_keys
                .push(format!("subgraph-{subgraph_name}:key-{key}"));
        }
        document
            .invalidation_keys
            .push(format!("subgraph-{subgraph_name}"));

        // apply needed prefixes
        document.invalidation_keys = document
            .invalidation_keys
            .drain(..)
            .map(|key| self.make_key(format!("cache-tag:{key}")))
            .collect();

        // TODO: what if this entity doesn't have the same cache tags as one that was otherwise identical? is that part of the hash algorithm?

        // TODO: what if this fails in execution (not in queuing)?
        for key in &document.invalidation_keys {
            // TODO: set expire time for sorted set?
            let _: () = pipeline
                .zadd(
                    key,
                    None,
                    Some(Ordering::GreaterThan),
                    false,
                    false,
                    vec![(expire_at as f64, pck.clone())],
                )
                .await?;
        }

        let cache_tags_key = self.make_key(Self::cache_tags_key(&document.cache_key));
        // the cache_tags_key is effectively a set of values, but atomic operations (ie GETDEL) are only
        // possible on a string; plus, we don't need the set primitives for this key.
        let _: () = pipeline
            .set(
                cache_tags_key,
                &serde_json::to_string(&document.invalidation_keys).unwrap(),
                Some(Expiration::EXAT(expire_at_doubled as i64)), // expire much later than the key itself to allow time for cleanup
                None,
                false,
            )
            .await?;

        Ok(())
    }

    async fn invalidate_internal(&self, invalidation_keys: Vec<String>) -> StorageResult<u64> {
        const SCRIPT: &str = include_str!("invalidate_key.lua");

        let mut all_keys = HashSet::new();

        // TODO: make sure this actually iterates over all nodes in the cluster
        let client = self.storage.client();
        for server in self.storage.servers() {
            for invalidation_key in &invalidation_keys {
                let invalidation_key = self.make_key(format!("cache-tag:{invalidation_key}"));
                let keys: Vec<String> = client
                    .with_cluster_node(server.clone())
                    .eval(SCRIPT, invalidation_key, ())
                    .await?;
                all_keys.extend(keys.into_iter().map(fred::types::Key::from));
            }
        }

        if all_keys.is_empty() {
            return Ok(0);
        }

        let count = self.storage.delete_from_scan_result(all_keys).await;
        Ok(count.unwrap_or(0) as u64)
    }

    pub(crate) async fn enable_keyevent_notifications(&self) {
        // TODO
        let client = self.keyspace_storage.client();
        for server in self.keyspace_storage.servers() {
            client
                .with_cluster_node(server.clone())
                .config_set("notify-keyspace-events", "Exeg")
                .await
                .unwrap_or_else(|_| panic!("Failed to configure server {server:?}"));
        }
    }

    pub(crate) async fn subscribe_to_keyspace_events(&self) {
        let prefixes = vec![
            String::from("__key*__:del"),
            String::from("__key*__:expired"),
            String::from("__key*__:evicted"),
        ];
        // TODO: configure keyspace storage with pool size of 1 ?
        // https://github.com/aembke/fred.rs/blob/main/examples/keyspace.rs#L84
        let subscription_client = self.keyspace_storage.client();
        let reconnect_subscriber = subscription_client.clone();
        let prefixes_clone = prefixes.clone();
        // resubscribe to PREFIXES whenever we reconnect to a server
        let _reconnect_task = tokio::spawn(async move {
            let mut reconnect_rx = reconnect_subscriber.reconnect_rx();

            // in 7.x the reconnection interface added a `Server` struct to reconnect events to make this easier.
            while let Ok(server) = reconnect_rx.recv().await {
                tracing::debug!(
                    "Reconnected to {}. Subscribing to keyspace events...",
                    server
                );
                // TODO: actually handle failures here
                let _ = reconnect_subscriber
                    .with_cluster_node(server)
                    .psubscribe(prefixes_clone.clone())
                    .await;
            }
        });

        let storage = self.storage.clone();

        // set up a task that listens for keyspace events
        let pck_prefix = self.pck_prefix();
        let _keyspace_task = tokio::spawn(async move {
            // have to subscribe!!
            let _ = subscription_client.psubscribe(prefixes).await;

            let mut keyspace_rx = subscription_client.keyspace_event_rx();
            loop {
                match keyspace_rx.recv().await {
                    Ok(event) => {
                        if (event.operation == "del"
                            || event.operation == "expired"
                            || event.operation == "evicted")
                            && (event.key.as_str_lossy().starts_with(&pck_prefix))
                        {
                            // need a non-subscription client! aka cannot just use this client..
                            let client = storage.client();
                            tokio::spawn(async move {
                                let _ = handle_pck_deletion(
                                    client,
                                    event.key.as_str_lossy().to_string(),
                                )
                                .await;

                                // TODO: report success / failure rate
                            });
                        }
                    }
                    Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(_)) => continue,
                }
            }
        });
    }

    pub(crate) async fn perform_periodic_maintenance(&self) {
        let storage = self.storage.clone();
        // leave off namespace as that's handled by the storage layer
        let key_pattern = String::from("cache-tag:*");

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                let _ = interval.tick().await;
                let now = now_epoch_seconds();
                let cutoff = now;

                let pipeline = storage.pipeline();
                let mut scan_stream =
                    storage.scan_with_namespaced_results(key_pattern.clone(), Some(10));
                while let Some(scan_result) = scan_stream.next().await {
                    match scan_result {
                        Ok(mut scan_result) => {
                            if let Some(keys) = scan_result.take_results() {
                                for key in keys {
                                    // TODO: make this a separate fn so I don't have to use unwrap
                                    let _: () = pipeline
                                        .zremrangebyscore(key, f64::NEG_INFINITY, cutoff as f64)
                                        .await
                                        .unwrap();
                                }
                            }
                            scan_result.next();
                        }
                        Err(err) => {
                            tracing::warn!("caught error: {err:?}");
                            break;
                        }
                    }
                }

                let result: Result<Vec<u64>, _> = pipeline.all().await;
                if let Ok(result) = result {
                    let _total: u64 = result.into_iter().sum();
                    // TODO: error handling, metric for total
                }
            }
        });
    }
}

async fn handle_pck_deletion(client: Client, key: String) -> StorageResult<()> {
    // TODO: how to handle cache-key-* being on a different shard than the pck? cannot have cross-shard transactions
    // script will atomically watch PCK and only if it does not exist will it call GETDEL
    const SCRIPT: &str = include_str!("clean_up_pck.lua");

    let cache_tags_key = key.clone().replacen("pck:", "cache-tags:", 1);

    let result: Value = client
        .eval(SCRIPT, (key.clone(), cache_tags_key), ())
        .await?;
    let keys = result.as_string().ok_or(Error::Placeholder)?;

    // let keys: String = client.eval(SCRIPT, (key, cache_tags_key), ()).await?;
    let cache_tags: Vec<String> = serde_json::from_str(&keys)?;

    // TODO: this is the part that probably should still be atomic but won't be for now...
    let pipeline = client.pipeline();
    for cache_tag in cache_tags {
        let _: () = pipeline.zrem(cache_tag, key.clone()).await?;
    }
    let _: Vec<Value> = pipeline.all().await?;
    Ok(())
}

impl CacheStorage for Storage {
    async fn _insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        let pipeline = self.storage.pipeline();
        self.add_insert_to_pipeline(&pipeline, document, now_epoch_seconds(), subgraph_name)
            .await?;
        let _: () = pipeline.last().await?;
        Ok(())
    }

    async fn _insert_in_batch(
        &self,
        batch_docs: Documents,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        let pipeline = self.storage.pipeline();
        let now = now_epoch_seconds();
        // todo: right now this will abort if any of the docs fail - is this desirable?
        for document in batch_docs {
            self.add_insert_to_pipeline(&pipeline, document, now, subgraph_name)
                .await?;
        }
        let _: () = pipeline.last().await?;
        Ok(())
    }

    async fn _get(&self, cache_key: &str) -> StorageResult<CacheEntry> {
        // don't need make_key for gets etc as the storage layer already runs it
        let key = RedisKey(Self::primary_cache_key(cache_key));
        // TODO: it would be nice for the storage layer to return errors or smth
        let value: RedisValue<CacheValue> = self.storage.get(key).await.ok_or(
            fred::error::Error::new(fred::error::ErrorKind::NotFound, ""),
        )?;

        Ok(CacheEntry::try_from((cache_key, value.0))?)
    }

    async fn _get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>> {
        let keys: Vec<RedisKey<String>> = cache_keys
            .iter()
            .map(|key| RedisKey(Self::primary_cache_key(key)))
            .collect();
        let values: Vec<Option<RedisValue<CacheValue>>> = self
            .storage
            .get_multiple(keys)
            .await
            .ok_or(fred::error::Error::new(
                fred::error::ErrorKind::NotFound,
                "",
            ))?;

        let entries = values
            .into_iter()
            .zip(cache_keys)
            .map(|(value, cache_key)| {
                if let Some(value) = value {
                    CacheEntry::try_from((*cache_key, value.0)).ok()
                } else {
                    None
                }
            })
            .collect();

        Ok(entries)
    }

    async fn _invalidate_by_subgraphs(&self, subgraph_names: Vec<String>) -> StorageResult<u64> {
        let keys = subgraph_names
            .into_iter()
            .map(|n| format!("subgraph-{n}"))
            .collect();
        self.invalidate_internal(keys).await
    }

    async fn _invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> StorageResult<HashMap<String, u64>> {
        let mut tasks = Vec::new();
        for subgraph_name in &subgraph_names {
            let keys: Vec<String> = invalidation_keys
                .iter()
                .map(|invalidation_key| format!("subgraph-{subgraph_name}:key-{invalidation_key}"))
                .collect();
            tasks.push(self.invalidate_internal(keys));
        }

        let counts = join_all(tasks).await;

        Ok(subgraph_names
            .into_iter()
            .zip(counts.into_iter())
            .map(|(name, result)| (name, result.unwrap_or(0)))
            .collect())
    }

    #[cfg(test)]
    async fn truncate_namespace(&self) -> StorageResult<()> {
        self.storage.truncate_namespace().await;
        Ok(())
    }
}

#[cfg(test)]
#[allow(unused)]
pub(crate) fn default_redis_cache_config() -> Config {
    use std::time::Duration;
    Config {
        urls: vec!["redis://127.0.0.1:6379".parse().unwrap()],
        username: None,
        password: None,
        timeout: Some(Duration::from_millis(5)),
        ttl: None,
        namespace: None,
        tls: None,
        required_to_start: true,
        reset_ttl: false,
        pool_size: 1,
        metrics_interval: Duration::from_secs(1),
    }
}
