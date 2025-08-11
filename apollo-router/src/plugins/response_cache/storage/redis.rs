use std::collections::HashMap;
use std::collections::HashSet;

use fred::clients::Client;
use fred::clients::Pipeline;
use fred::interfaces::KeysInterface;
use fred::interfaces::LuaInterface;
use fred::interfaces::SetsInterface;
use fred::interfaces::SortedSetsInterface;
use fred::types::Expiration;
use fred::types::sorted_sets::Ordering;
use futures::future::join_all;
use serde::Deserialize;
use serde::Serialize;
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
}

impl Storage {
    pub(crate) async fn new(config: &Config) -> Result<Self, BoxError> {
        let storage = RedisCacheStorage::new(config.clone(), "response-cache").await?;
        Ok(Storage { storage })
    }

    fn make_key(&self, key: String) -> String {
        self.storage.make_key(RedisKey(key))
    }

    async fn add_insert_to_pipeline(
        &self,
        pipeline: &Pipeline<Client>,
        mut document: Document,
        now: u64,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        let expire_at = now + document.expire.as_secs();
        let pck = self.make_key(format!("pck:{}", document.cache_key));
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

        for key in &document.invalidation_keys {
            // TODO: set expire time
            let _: () = pipeline
                .zadd(
                    self.make_key(format!("cache-tag:{key}")),
                    None,
                    Some(Ordering::GreaterThan),
                    false,
                    false,
                    vec![(expire_at as f64, pck.clone())],
                )
                .await?;
        }

        let cache_tags_key = self.make_key(format!("cache-tags:{}", document.cache_key));
        let _: () = pipeline.del(cache_tags_key.clone()).await?;
        let _: () = pipeline
            .sadd(cache_tags_key, document.invalidation_keys)
            .await?;

        Ok(())
    }

    async fn invalidate_internal(&self, invalidation_keys: Vec<String>) -> StorageResult<u64> {
        const SCRIPT: &str = "local key = KEYS[1]; local members = redis.call('ZRANGE', key, 0, -1); redis.call('DEL', key); return members";

        let mut all_keys = HashSet::new();
        // TODO: make sure this is all the clients in the cluster
        for client in self.storage.all_clients() {
            for invalidation_key in &invalidation_keys {
                let invalidation_key = self.make_key(format!("cache-tag:{invalidation_key}"));
                let keys: Vec<String> = client.eval(SCRIPT, invalidation_key, ()).await?;
                all_keys.extend(keys.into_iter().map(fred::types::Key::from));
            }
        }

        if all_keys.is_empty() {
            return Ok(0);
        }

        let count = self.storage.delete_from_scan_result(all_keys).await;
        Ok(count.unwrap_or(0) as u64)
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
        for document in batch_docs {
            self.add_insert_to_pipeline(&pipeline, document, now, subgraph_name)
                .await?;
        }
        let _: () = pipeline.last().await?;
        Ok(())
    }

    async fn _get(&self, cache_key: &str) -> StorageResult<CacheEntry> {
        // don't need make_key for gets etc as the storage layer already runs it
        let value: RedisValue<CacheValue> = self
            .storage
            .get(RedisKey(format!("pck:{cache_key}")))
            .await
            .ok_or(fred::error::Error::new(
                fred::error::ErrorKind::NotFound,
                "",
            ))?;

        Ok(CacheEntry::try_from((cache_key, value.0))?)
    }

    async fn _get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>> {
        let keys: Vec<RedisKey<String>> = cache_keys
            .iter()
            .map(|key| RedisKey(format!("pck:{key}")))
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
