use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;

use fred::clients::Client;
use fred::clients::Pipeline;
use fred::interfaces::KeysInterface;
use fred::interfaces::LuaInterface;
use fred::interfaces::SortedSetsInterface;
use fred::types::Expiration;
use fred::types::scan::Scanner;
use fred::types::sorted_sets::Ordering;
use futures::StreamExt;
use futures::future::join_all;
use serde::Deserialize;
use serde::Serialize;
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
            // TODO: set expire time for sorted set? might not be needed thanks to periodic mx
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

    pub(crate) async fn perform_periodic_maintenance(&self) {
        let storage = self.storage.clone();
        // leave off namespace as that's handled by the storage layer
        let key_pattern = String::from("cache-tag:*");

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                let _ = interval.tick().await;
                let now = Instant::now();
                let cutoff = now_epoch_seconds() - 1;

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

                let elapsed = now.elapsed().as_secs_f64();
                f64_histogram_with_unit!(
                    "apollo.router.operations.response_cache.storage.maintenance",
                    "Time to perform cache tag maintenance",
                    "s",
                    elapsed
                );
            }
        });
    }
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
