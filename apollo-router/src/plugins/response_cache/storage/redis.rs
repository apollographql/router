use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;

use fred::clients::Client;
use fred::clients::Pipeline;
use fred::interfaces::KeysInterface;
use fred::interfaces::SortedSetsInterface;
use fred::types::Expiration;
use fred::types::ExpireOptions;
use fred::types::sorted_sets::Ordering;
use futures::future::join_all;
use serde::Deserialize;
use serde::Serialize;
use tokio::time::MissedTickBehavior;
use tower::BoxError;

use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::cache::storage::KeyType;
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
    long_storage: RedisCacheStorage,
}

impl Storage {
    pub(crate) async fn new(config: &Config) -> Result<Self, BoxError> {
        // TODO: make the 'caller' parameter include the namespace? or subgraph name?
        let storage = RedisCacheStorage::new(config.clone(), "response-cache").await?;

        let mut long_config = config.clone();
        long_config.timeout = Some(Duration::from_secs(10));
        let long_storage = RedisCacheStorage::new(long_config, "response-cache-long").await?;

        // TODO: make these actually have separate configs - this is just a hack for now

        let s = Storage {
            storage,
            long_storage,
        };

        s.perform_periodic_maintenance().await;

        Ok(s)
    }

    fn make_key<K: KeyType>(&self, key: K) -> String {
        self.storage.make_key(RedisKey(key))
    }

    async fn add_insert_to_pipeline(
        &self,
        pipeline: &Pipeline<Client>,
        mut document: Document,
        now: u64,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        // idea: maybe store expire times internally so we don't have to update them if not necessary?
        // TODO: better error handling for all of this...
        let key_expire_at = now + document.expire.as_secs();
        let cache_tag_expire_at = key_expire_at + 1;

        let pck = self.make_key(&document.cache_key);
        let value = CacheValue {
            data: document.data,
            cache_control: document.cache_control,
        };

        // TODO: figure out if this is actually how we want to store the values
        let _: () = pipeline
            .set(
                pck.clone(),
                &serde_json::to_string(&value).unwrap(),
                Some(Expiration::EXAT(key_expire_at as i64)),
                None,
                false,
            )
            .await?;

        // add 'subgraph' to invalidation keys
        // TODO: ask @bnjjj if we need to do this - ie do we need to support whole-subgraph invalidation, and do we need to support
        //  only invalidating a certain subgraph for a key
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

        // apply needed prefixes (ie namespace, `cache-tag:`)
        document.invalidation_keys = document
            .invalidation_keys
            .drain(..)
            .map(|key| self.make_key(format!("cache-tag:{key}")))
            .collect();

        // TODO: what if this entity doesn't have the same cache tags as one that was otherwise identical? is that part of the hash algorithm?
        //  .. make sure cache tags are part of the hash

        // TODO: what if this fails in execution (not in queuing)?
        for key in &document.invalidation_keys {
            // NB: both of these make use of the GT ordering, so the sort values are only updated
            // if the value is greater than the existing one (ie it takes the max of the expiration time)

            let _: () = pipeline
                .zadd(
                    key,
                    None,
                    Some(Ordering::GreaterThan),
                    false,
                    false,
                    vec![(key_expire_at as f64, pck.clone())],
                )
                .await?;

            // NB: this should be ok with volatile-ttl eviction as it means the cache tag will always have a greater
            // TTL than any of its keys
            let _: () = pipeline
                .expire_at(key, cache_tag_expire_at as i64, Some(ExpireOptions::GT))
                .await?;
        }

        let cache_tags_with_exp: Vec<_> = document
            .invalidation_keys
            .into_iter()
            .map(|key| (cache_tag_expire_at as f64, key))
            .collect();
        let _: () = pipeline
            .zadd(
                self.make_key("cache-tags"),
                None,
                Some(Ordering::GreaterThan),
                false,
                false,
                cache_tags_with_exp,
            )
            .await?;

        Ok(())
    }

    async fn invalidate_internal(&self, invalidation_keys: Vec<String>) -> StorageResult<u64> {
        let mut all_keys = HashSet::new();

        // TODO: parallelize this
        for invalidation_key in &invalidation_keys {
            let client = self.long_storage.client();
            let invalidation_key = self.make_key(format!("cache-tag:{invalidation_key}"));
            let keys_with_scores: Vec<String> = client
                .zrange(invalidation_key.clone(), 0, -1, None, false, None, false)
                .await?;
            all_keys.extend(keys_with_scores.into_iter().map(fred::types::Key::from));
        }

        if all_keys.is_empty() {
            return Ok(0);
        }

        // TODO: make redis storage impl actually return a vec of results - or don't use redis storage impl for the delete
        //  checking whether count == expected_deletions isn't a good way to check success since some keys may TTL by the
        //  time we actually call the delete
        let expected_deletions = all_keys.len() as u64;
        let count = self
            .long_storage
            .delete_from_scan_result(all_keys)
            .await
            .ok_or(fred::error::Error::new(
                fred::error::ErrorKind::Unknown,
                "not sure how we got here",
            ))? as u64;

        tracing::info!("invalidated {count} keys of {expected_deletions} expected");

        // NOTE: we don't delete elements from the cache tag sorted sets. doing so could get us in trouble
        // with race conditions, etc. it's safer to just rely on the TTL-based cleanup.
        Ok(count)
    }

    pub(crate) async fn perform_periodic_maintenance(&self) {
        let storage = self.long_storage.clone();
        let key = self.make_key("cache-tags");

        // maintenance 1: take random members from cache-tags and use zremrangebyscore on them
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                let _ = interval.tick().await;
                let now = Instant::now();
                let cutoff = now_epoch_seconds() - 1;

                // fetch random cache tag
                let cache_tag_key: String =
                    match storage.client().zrandmember(&key, Some((1, false))).await {
                        Ok(Some(s)) => s,
                        Ok(None) => {
                            // TODO error handling
                            tracing::debug!("no cache tags available to perform maintenance");
                            continue;
                        }
                        Err(err) => {
                            // TODO error handling
                            eprintln!("error while fetching cache tag: {err:?}");
                            continue;
                        }
                    };

                let removed_items: u64 = storage
                    .client()
                    .zremrangebyscore(&cache_tag_key, f64::NEG_INFINITY, cutoff as f64)
                    .await
                    .unwrap_or_else(|err| {
                        // TODO error handling
                        eprintln!("error while removing keys from cache-tag: {err:?}");
                        0
                    });

                u64_counter!(
                    "apollo.router.operations.response_cache.storage.maintenance.removed_cache_tag_entries",
                    "Counter for removed items",
                    removed_items
                );

                let elapsed = now.elapsed().as_secs_f64();
                f64_histogram_with_unit!(
                    "apollo.router.operations.response_cache.storage.maintenance",
                    "Time to perform cache tag maintenance",
                    "s",
                    elapsed
                );
            }
        });

        // maintenance 2: use zremrangebyscore on cache-tags
        let storage = self.storage.clone();
        let key = self.make_key("cache-tags");
        tokio::spawn(async move {
            let key = key.clone();
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                let _ = interval.tick().await;
                let cutoff = now_epoch_seconds() - 1;

                let removed_items: u64 = storage
                    .client()
                    .zremrangebyscore(&key, f64::NEG_INFINITY, cutoff as f64)
                    .await
                    .unwrap_or_else(|err| {
                        // TODO error handling
                        eprintln!("error while removing cache-tags: {err:?}");
                        0
                    });

                u64_counter!(
                    "apollo.router.operations.response_cache.storage.maintenance.removed_cache_tags",
                    "Counter for removed items",
                    removed_items
                );
            }
        });
    }
}

impl CacheStorage for Storage {
    async fn _insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        let pipeline = self.long_storage.pipeline();
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
        // let client = self.storage.

        let pipeline = self.long_storage.pipeline();
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
        let key = RedisKey(cache_key);
        // TODO: it would be nice for the storage layer to return errors or smth
        let value: RedisValue<CacheValue> = self.storage.get(key).await.ok_or(
            fred::error::Error::new(fred::error::ErrorKind::NotFound, ""),
        )?;

        Ok(CacheEntry::try_from((cache_key, value.0))?)
    }

    async fn _get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>> {
        let keys: Vec<RedisKey<String>> = cache_keys
            .iter()
            .map(|key| RedisKey(key.to_string()))
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
