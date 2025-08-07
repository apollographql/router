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
use serde_json::json;
use tower::BoxError;

use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::plugins::response_cache::cache_control::now_epoch_seconds;
use crate::plugins::response_cache::storage::CacheEntry;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::Document;
use crate::plugins::response_cache::storage::StorageResult;

pub(crate) type Config = crate::configuration::RedisCache;

// TODO: make this have better types if we only use redis..
#[derive(Deserialize)]
struct CacheValue {
    data: String,
    control: String,
}

impl TryFrom<(&str, CacheValue)> for CacheEntry {
    type Error = serde_json::Error;

    fn try_from((cache_key, cache_value): (&str, CacheValue)) -> Result<Self, Self::Error> {
        Ok(CacheEntry {
            cache_key: cache_key.to_string(),
            data: serde_json::from_str(&cache_value.data)?,
            control: serde_json::from_str(&cache_value.control)?,
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

        // TODO: figure out if this is actually how we want to store the values
        let _: () = pipeline
            .set(
                self.make_key(document.cache_key.clone()),
                json!({"value": document.data, "control": document.control}).to_string(),
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
                    vec![(expire_at as f64, document.cache_key.clone())],
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
        let mut all_keys = HashSet::new();
        for client in self.storage.all_clients() {
            for invalidation_key in &invalidation_keys {
                let invalidation_key = self.make_key(invalidation_key.clone());

                let script = r#""""local key = KEYS[1]; local members = redis.call('ZRANGE', key, 0, -1, 'WITHSCORES'); redis.call('DEL', key); return members""""#;

                let keys: Vec<String> = client.eval(script, 1, invalidation_key).await?;
                all_keys.extend(keys);
            }
        }

        let pipeline = self.storage.pipeline();
        for key in all_keys {
            let _: () = pipeline.del(self.make_key(key)).await?;
        }

        let counts: Vec<u64> = pipeline.all().await?;
        Ok(counts.into_iter().sum())
    }
}

impl CacheStorage for Storage {
    async fn insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        let pipeline = self.storage.pipeline();
        self.add_insert_to_pipeline(&pipeline, document, now_epoch_seconds(), subgraph_name)
            .await?;
        let _: () = pipeline.last().await?;
        Ok(())
    }

    async fn insert_in_batch(
        &self,
        batch_docs: Vec<Document>,
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

    async fn get(&self, cache_key: &str) -> StorageResult<CacheEntry> {
        // don't need make_key for gets etc as the storage layer already runs it
        let value: RedisValue<String> =
            self.storage
                .get(RedisKey(cache_key))
                .await
                .ok_or(fred::error::Error::new(
                    fred::error::ErrorKind::NotFound,
                    "",
                ))?;

        let parsed_value: CacheValue = serde_json::from_str(&value.0)?;
        Ok(CacheEntry::try_from((cache_key, parsed_value))?)
    }

    async fn get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>> {
        let keys: Vec<RedisKey<String>> = cache_keys
            .iter()
            .map(|key| RedisKey(key.to_string()))
            .collect();
        let values: Vec<Option<RedisValue<String>>> =
            self.storage
                .get_multiple(keys)
                .await
                .ok_or(fred::error::Error::new(
                    fred::error::ErrorKind::NotFound,
                    "",
                ))?;

        let entries = values
            .into_iter()
            .map(|value| {
                if let Some(value) = value {
                    let parsed_value: Option<CacheValue> = serde_json::from_str(&value.0).ok();
                    parsed_value
                } else {
                    None
                }
            })
            .zip(cache_keys)
            .map(|(parsed_value, cache_key)| {
                if let Some(parsed_value) = parsed_value {
                    CacheEntry::try_from((*cache_key, parsed_value)).ok()
                } else {
                    None
                }
            })
            .collect();

        Ok(entries)
    }

    async fn invalidate_by_subgraphs(&self, subgraph_names: Vec<String>) -> StorageResult<u64> {
        let keys = subgraph_names
            .into_iter()
            .map(|n| format!("subgraph-{n}"))
            .collect();
        self.invalidate_internal(keys).await
    }

    async fn invalidate(
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

    async fn expired_data_count(&self) -> StorageResult<u64> {
        // intentional no-op
        Ok(0)
    }
}
