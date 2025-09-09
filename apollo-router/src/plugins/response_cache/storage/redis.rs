use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;

use fred::interfaces::KeysInterface;
use fred::interfaces::SortedSetsInterface;
use fred::types::Expiration;
use fred::types::ExpireOptions;
use fred::types::Value;
use fred::types::sorted_sets::Ordering;
use futures::future::join_all;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
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

// TODO: better docs throughout this
// TODO: need to suggest how to configure replication lag

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
    reader_storage: RedisCacheStorage,
    writer_storage: RedisCacheStorage,
    cache_tag_tx: mpsc::Sender<String>,
    timeout: Duration,
}

impl Storage {
    pub(crate) async fn new(config: &Config) -> Result<Self, BoxError> {
        // NB: sorted set cleanup happens via an async task, reading from `cache_tag_rx`.
        //  Items are added to it via `try_send` to avoid blocking, but this does mean that some items
        //  won't be added to the channel. This is probably acceptable given the limited number of options
        //  for the cache tag:
        //   * frequently used - another insert will eventually add the cache tag to the queue
        //   * not frequently used - small memory footprint, so probably doesn't need much cleanup
        //   * never used again - will be removed via TTL
        //  There are opportunities for improvement here to make sure that we don't try to do maintenance
        //  on the same cache tag multiple times a second, and perhaps a world where we actually want multiple
        //  consumers running at the same time.
        // TODO: make channel size configurable?
        let (cache_tag_tx, cache_tag_rx) = mpsc::channel(10000);

        let pool_size = config.pool_size / 2;
        let config = Config {
            pool_size,
            ..config.clone()
        };

        // TODO: make the 'caller' parameter include the namespace? or subgraph name?
        let s = Storage {
            timeout: config.timeout.unwrap_or_else(|| Duration::from_secs(1)), // TODO: self.timeout should be optional, but hack for now
            reader_storage: RedisCacheStorage::new(config.clone(), "response-cache-reader").await?,
            writer_storage: RedisCacheStorage::new(config, "response-cache-writer").await?,
            cache_tag_tx,
        };

        s.perform_periodic_maintenance(cache_tag_rx).await;

        Ok(s)
    }

    fn make_key<K: KeyType>(&self, key: K) -> String {
        self.reader_storage.make_key(RedisKey(key))
    }

    async fn invalidate_internal(&self, invalidation_keys: Vec<String>) -> StorageResult<u64> {
        let mut tasks = Vec::new();
        // TODO: parallelize this
        for invalidation_key in &invalidation_keys {
            let client = self.writer_storage.client();
            let invalidation_key = self.make_key(format!("cache-tag:{invalidation_key}"));
            tasks.push(async move {
                client
                    .zrange::<Vec<String>, _, _, _>(
                        invalidation_key.clone(),
                        0,
                        -1,
                        None,
                        false,
                        None,
                        false,
                    )
                    .await
            });
        }

        let mut all_keys = HashSet::new();
        let results = join_all(tasks).await;
        for result in results {
            all_keys.extend(result?.into_iter().map(fred::types::Key::from));
        }

        if all_keys.is_empty() {
            return Ok(0);
        }

        let expected_deletions = all_keys.len() as u64;
        let results = self.writer_storage.delete_from_scan_result(all_keys).await;

        let mut deleted = 0;
        let mut errors = 0;
        let mut error = None;
        for result in results {
            match result {
                Ok(count) => deleted += count as u64,
                Err(err) => {
                    errors += 1;
                    error = Some(err)
                }
            }
        }

        tracing::debug!(
            "invalidated {deleted} keys of {expected_deletions} expected, encountered {errors} errors"
        );

        // NOTE: we don't delete elements from the cache tag sorted sets. doing so could get us in trouble
        // with race conditions, etc. it's safer to just rely on the TTL-based cleanup.

        if let Some(error) = error {
            Err(error.into())
        } else {
            Ok(deleted)
        }
    }

    pub(crate) async fn perform_periodic_maintenance(&self, mut cache_tag_rx: Receiver<String>) {
        let storage = self.writer_storage.clone();

        // spawn a task that reads from cache_tag_rx and uses `zremrangebyscore` on each cache tag
        tokio::spawn(async move {
            while let Some(cache_tag) = cache_tag_rx.recv().await {
                let now = Instant::now();
                // NB: `cache_tag` already includes namespace
                let cache_tag_key = cache_tag;
                let cutoff = now_epoch_seconds() - 1;
                let removed_items_result: Result<u64, _> = storage
                    .client()
                    .zremrangebyscore(&cache_tag_key, f64::NEG_INFINITY, cutoff as f64)
                    .await;

                let elapsed = now.elapsed();
                f64_histogram_with_unit!(
                    "apollo.router.operations.response_cache.storage.maintenance",
                    "Time to perform maintenance on a cache tag",
                    "s",
                    elapsed.as_secs_f64()
                );

                match removed_items_result {
                    Ok(removed_items) => {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.storage.maintenance.removed_cache_tag_entries",
                            "Counter for removed items",
                            "{entry}",
                            removed_items
                        );
                    }
                    Err(err) => {
                        tracing::info!("Caught error while performing maintenance: {err:?}");
                    }
                }
            }
        });
    }
}

impl CacheStorage for Storage {
    fn timeout_duration(&self) -> Duration {
        self.timeout
    }

    async fn _insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        // TODO: optimize for this?
        self._insert_in_batch(vec![document], subgraph_name).await
    }

    async fn _insert_in_batch(
        &self,
        mut batch_docs: Documents,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        // three phases:
        //   1 - update potential keys to include namespace etc so that we don't have to do it in each phase
        //   2 - update each cache tag with new keys
        //   3 - update each key
        // a failure in any phase will cause the function to return, that prevents invalid states

        // TODO:
        //  * break these into separate fns
        //  * do things with metrics
        //  * break up batches into smaller batches...?

        let now = now_epoch_seconds();

        // phase 1
        for document in &mut batch_docs {
            document.cache_key = self.make_key(&document.cache_key);

            let invalidation_keys = document.invalidation_keys.clone();
            for invalidation_key in invalidation_keys {
                document
                    .invalidation_keys
                    .push(format!("subgraph-{subgraph_name}:key-{invalidation_key}"));
            }
            document
                .invalidation_keys
                .push(format!("subgraph-{subgraph_name}"));
            document.invalidation_keys = document
                .invalidation_keys
                .drain(..)
                .map(|key| self.make_key(format!("cache-tag:{key}")))
                .collect();
        }

        // phase 2
        let mut cache_tags_to_pcks: HashMap<String, Vec<(f64, String)>> = HashMap::default();
        for document in &mut batch_docs {
            for cache_tag_key in document.invalidation_keys.drain(..) {
                let entry = cache_tags_to_pcks.entry(cache_tag_key).or_default();
                entry.push((
                    (now + document.expire.as_secs()) as f64,
                    document.cache_key.clone(),
                ));
            }
        }

        let inst_now = Instant::now();

        // NB: spawn separate tasks in case sets are on different shards, as fred will multiplex into
        // pipelines anyway
        let mut tasks = Vec::new();
        // NB: client is shared across all inserts here
        let client = self.writer_storage.client();
        for (cache_tag_key, elements) in cache_tags_to_pcks.into_iter() {
            // NB: send this key to the queue for cleanup
            let _ = self.cache_tag_tx.try_send(cache_tag_key.clone());

            // work out expiration time to avoid setting it repeatedly
            let max_expiry_time = elements
                .iter()
                .map(|(exp_time, _)| *exp_time)
                .reduce(f64::max)
                .unwrap_or(now as f64)
                + 1.0;

            let pipeline = client.pipeline();
            tasks.push(async move {
                let _: Result<(), _> = pipeline
                    .zadd(
                        cache_tag_key.clone(),
                        None,
                        Some(Ordering::GreaterThan),
                        false,
                        false,
                        elements,
                    )
                    .await;

                // > A non-volatile key is treated as an infinite TTL for the purpose of GT and LT.
                // > The GT, LT and NX options are mutually exclusive.
                //   - https://redis.io/docs/latest/commands/expire/
                //
                // what we want are NX (set when key has no expiry) AND GT (set when new expiry is greater
                // than the current one.
                // which means we have to call expire_at twice :(

                let _: Result<(), _> = pipeline
                    .expire_at(
                        cache_tag_key.clone(),
                        max_expiry_time as i64,
                        Some(ExpireOptions::NX),
                    )
                    .await;
                let _: Result<(), _> = pipeline
                    .expire_at(
                        cache_tag_key,
                        max_expiry_time as i64,
                        Some(ExpireOptions::GT),
                    )
                    .await;
                pipeline.all().await
            });
        }

        let results: Vec<Result<Vec<Value>, _>> = join_all(tasks).await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.insert.duration",
            "Duration of parallel insert",
            "s",
            inst_now.elapsed().as_secs_f64(),
            phase = "phase 2 - zadd and expire_at"
        );

        for result in results {
            if let Err(err) = result {
                tracing::info!("Caught error during cache tag update: {err:?}");
                return Err(err.into());
            }
        }

        // phase 3
        let inst_now = Instant::now();

        // NB: spawn separate tasks in case sets are on different shards, as fred will multiplex into
        // pipelines anyway
        let mut tasks = Vec::new();
        for document in batch_docs.into_iter() {
            let client = self.writer_storage.client();
            let value = CacheValue {
                data: document.data,
                cache_control: document.cache_control,
            };
            tasks.push(async move {
                client
                    .set(
                        document.cache_key,
                        &serde_json::to_string(&value).unwrap(),
                        Some(Expiration::EXAT((now + document.expire.as_secs()) as i64)),
                        None,
                        false,
                    )
                    .await
            });
        }
        let results: Vec<Result<Value, _>> = join_all(tasks).await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.insert.duration",
            "Duration of parallel insert",
            "s",
            inst_now.elapsed().as_secs_f64(),
            phase = "phase 3 - insert values"
        );

        for result in results {
            if let Err(err) = result {
                tracing::info!("Caught error during document insert: {err:?}");
                return Err(err.into());
            }
        }

        tracing::debug!("Successfully inserted batch");

        Ok(())
    }

    async fn _get(&self, cache_key: &str) -> StorageResult<CacheEntry> {
        // don't need make_key for gets etc as the storage layer already runs it
        let value: RedisValue<CacheValue> = self.reader_storage.get(RedisKey(cache_key)).await?;
        Ok(CacheEntry::try_from((cache_key, value.0))?)
    }

    async fn _get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>> {
        let keys: Vec<RedisKey<String>> = cache_keys
            .iter()
            .map(|key| RedisKey(key.to_string()))
            .collect();
        let values: Vec<Option<RedisValue<CacheValue>>> =
            self.reader_storage.get_multiple(keys).await;

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
        self.reader_storage.truncate_namespace().await;
        self.writer_storage.truncate_namespace().await;
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
