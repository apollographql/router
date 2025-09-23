use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use fred::interfaces::KeysInterface;
use fred::interfaces::SortedSetsInterface;
use fred::types::Expiration;
use fred::types::ExpireOptions;
use fred::types::Value;
use fred::types::sorted_sets::Ordering;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tower::BoxError;

use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::cache::storage::KeyType;
use crate::cache::storage::ValueType;
use crate::plugins::response_cache::cache_control::CacheControl;
use crate::plugins::response_cache::storage::CacheEntry;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::Document;
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
            key: cache_key.to_string(),
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
        let pool_size = (config.pool_size / 2).max(1);
        let config = Config {
            pool_size,
            ..config.clone()
        };

        let reader_storage =
            RedisCacheStorage::new(config.clone(), "response-cache-reader").await?;
        let writer_storage =
            RedisCacheStorage::new(config.clone(), "response-cache-writer").await?;
        Self::from_storage(reader_storage, writer_storage, config.timeout).await
    }

    async fn from_storage(
        reader_storage: RedisCacheStorage,
        writer_storage: RedisCacheStorage,
        timeout: Duration,
    ) -> Result<Self, BoxError> {
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
        let s = Self {
            timeout,
            reader_storage,
            writer_storage,
            cache_tag_tx,
        };
        s.perform_periodic_maintenance(cache_tag_rx).await;
        Ok(s)
    }

    fn make_key<K: KeyType>(&self, key: K) -> String {
        self.reader_storage.make_key(RedisKey(key))
    }

    async fn invalidate_internal(&self, invalidation_keys: Vec<String>) -> StorageResult<u64> {
        let pipeline = self.writer_storage.pipeline();
        for invalidation_key in &invalidation_keys {
            let invalidation_key = self.make_key(format!("cache-tag:{invalidation_key}"));
            let _ = self.cache_tag_tx.try_send(invalidation_key.clone());
            let _: () = pipeline
                .zrange(invalidation_key.clone(), 0, -1, None, false, None, false)
                .await?;
        }

        let mut all_keys = HashSet::new();
        let result_vec: Vec<Result<Vec<String>, _>> = pipeline.try_all().await;
        for result in result_vec {
            all_keys.extend(result?.into_iter().map(fred::types::Key::from))
        }

        if all_keys.is_empty() {
            return Ok(0);
        }

        let deleted = self
            .writer_storage
            .delete_from_scan_result(all_keys.into_iter().collect())
            .await?;

        // NOTE: we don't delete elements from the cache tag sorted sets. doing so could get us in trouble
        // with race conditions, etc. it's safer to just rely on the TTL-based cleanup.
        Ok(deleted as u64)
    }

    pub(crate) async fn perform_periodic_maintenance(
        &self,
        mut cache_tag_rx: mpsc::Receiver<String>,
    ) {
        let storage = self.clone();

        // spawn a task that reads from cache_tag_rx and uses `zremrangebyscore` on each cache tag
        tokio::spawn(async move {
            while let Some(cache_tag) = cache_tag_rx.recv().await {
                // NB: `cache_tag` already includes namespace
                let cutoff = now() - 1;

                // TODO: timeout for this?
                let now = Instant::now();
                let removed_items_result = storage
                    .remove_keys_from_cache_tag_by_cutoff(cache_tag, cutoff as f64)
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
                        tracing::debug!("Caught error while performing maintenance: {err:?}");
                    }
                }
            }
        });
    }

    async fn remove_keys_from_cache_tag_by_cutoff(
        &self,
        cache_tag_key: String,
        cutoff_time: f64,
    ) -> StorageResult<u64> {
        // Returns number of items removed
        Ok(self
            .writer_storage
            .client()
            .zremrangebyscore(&cache_tag_key, f64::NEG_INFINITY, cutoff_time)
            .await?)
    }

    /// Create a list of the cache tags that describe this document, with associated namespaces.
    ///
    /// For a given subgraph `s` and invalidation keys `i1`, `i2`, ..., we need to store the
    /// following subgraph-invalidation-key permutations:
    /// * `subgraph-{s}` (whole subgraph)
    /// * `key-{i1}`, `key-{i2}`, ... (whole invalidation key)
    /// * `subgraph-{s}:key-{i1}`, `subgraph-{s}:key-{i2}`, ... (invalidation key per subgraph)
    ///
    /// These are then turned into redis keys by adding the namespace and a `cache-tag:` prefix, ie:
    /// * `{namespace}:cache-tag:subgraph-{s}`
    /// * `{namespace}:cache-tag:key-{i1}`, ...
    /// * `{namespace}:cache-tag:subgraph-{s}:key-{i1}`, ...
    fn namespaced_cache_tags(
        &self,
        document_invalidation_keys: &[String],
        subgraph_name: &str,
    ) -> Vec<String> {
        // TODO: test this
        let mut cache_tags = Vec::new();
        cache_tags.push(format!("subgraph-{subgraph_name}"));
        for invalidation_key in document_invalidation_keys {
            cache_tags.push(format!("key-{invalidation_key}"));
            cache_tags.push(format!("subgraph-{subgraph_name}:key-{invalidation_key}"));
        }

        for cache_tag in cache_tags.iter_mut() {
            *cache_tag = self.make_key(format!("cache-tag:{cache_tag}"));
        }

        cache_tags
    }
}

impl CacheStorage for Storage {
    fn insert_timeout(&self) -> Duration {
        self.timeout
    }

    fn fetch_timeout(&self) -> Duration {
        self.timeout
    }

    fn invalidate_timeout(&self) -> Duration {
        self.timeout
    }

    async fn internal_insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        self.internal_insert_in_batch(vec![document], subgraph_name)
            .await
    }

    async fn internal_insert_in_batch(
        &self,
        mut batch_docs: Vec<Document>,
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

        let now = now();

        // phase 1
        for document in &mut batch_docs {
            document.key = self.make_key(&document.key);
            document.invalidation_keys =
                self.namespaced_cache_tags(&document.invalidation_keys, subgraph_name);
        }

        // phase 2
        let mut cache_tags_to_pcks: HashMap<String, Vec<(f64, String)>> = HashMap::default();
        for document in &mut batch_docs {
            for cache_tag_key in document.invalidation_keys.drain(..) {
                let entry = cache_tags_to_pcks.entry(cache_tag_key).or_default();
                entry.push((
                    (now + document.expire.as_secs()) as f64,
                    document.key.clone(),
                ));
            }
        }

        // NB: spawn separate tasks in case sets are on different shards, as fred will multiplex into
        // pipelines anyway
        let pipeline = self.writer_storage.client().pipeline();
        for (cache_tag_key, elements) in cache_tags_to_pcks.into_iter() {
            // NB: send this key to the queue for cleanup
            let _ = self.cache_tag_tx.try_send(cache_tag_key.clone());

            // NB: expiry time being max + 1 is important! if you use a volatile TTL eviction policy,
            // Redis will evict the keys with the shortest TTLs - we have to make sure that the cache
            // tag will outlive any of the keys it refers to
            let max_expiry_time = elements
                .iter()
                .map(|(exp_time, _)| *exp_time)
                .reduce(f64::max)
                .unwrap_or(now as f64)
                + 1.0;

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
            // than the current one).
            // that means we have to call `expire_at` twice :(
            for exp_opt in [ExpireOptions::NX, ExpireOptions::GT] {
                let _: Result<(), _> = pipeline
                    .expire_at(cache_tag_key.clone(), max_expiry_time as i64, Some(exp_opt))
                    .await;
            }
        }

        let result_vec = pipeline.try_all::<Value>().await;
        for result in result_vec {
            if let Err(err) = result {
                tracing::debug!("Caught error during cache tag update: {err:?}");
                return Err(err.into());
            }
        }

        // phase 3
        let pipeline = self.writer_storage.client().pipeline();
        for document in batch_docs.into_iter() {
            let value = CacheValue {
                data: document.data,
                cache_control: document.control,
            };
            let _: () = pipeline
                .set::<(), _, _>(
                    document.key,
                    &serde_json::to_string(&value).unwrap(),
                    Some(Expiration::EXAT((now + document.expire.as_secs()) as i64)),
                    None,
                    false,
                )
                .await?;
        }

        let result_vec = pipeline.try_all::<Value>().await;
        for result in result_vec {
            if let Err(err) = result {
                tracing::debug!("Caught error during document insert: {err:?}");
                return Err(err.into());
            }
        }

        Ok(())
    }

    async fn internal_fetch(&self, cache_key: &str) -> StorageResult<CacheEntry> {
        // don't need make_key for gets etc as the storage layer already runs it
        let value: RedisValue<CacheValue> = self.reader_storage.get(RedisKey(cache_key)).await?;
        Ok(CacheEntry::try_from((cache_key, value.0))?)
    }

    async fn internal_fetch_multiple(
        &self,
        cache_keys: &[&str],
    ) -> StorageResult<Vec<Option<CacheEntry>>> {
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

    async fn internal_invalidate_by_subgraphs(
        &self,
        subgraph_names: Vec<String>,
    ) -> StorageResult<u64> {
        let keys = subgraph_names
            .into_iter()
            .map(|n| format!("subgraph-{n}"))
            .collect();
        self.invalidate_internal(keys).await
    }

    async fn internal_invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> StorageResult<HashMap<String, u64>> {
        let mut join_set = JoinSet::default();
        for subgraph_name in subgraph_names {
            let keys: Vec<String> = invalidation_keys
                .iter()
                .map(|invalidation_key| format!("subgraph-{subgraph_name}:key-{invalidation_key}"))
                .collect();
            let storage = self.clone();
            join_set.spawn(async move { (subgraph_name, storage.invalidate_internal(keys).await) });
        }

        let mut counts = HashMap::default();
        while let Some(result) = join_set.join_next().await {
            let (subgraph_name, count) = result?;
            counts.insert(subgraph_name, count.unwrap_or(0));
        }

        Ok(counts)
    }

    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    async fn truncate_namespace(&self) -> StorageResult<()> {
        self.writer_storage.truncate_namespace().await?;
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl Config {
    pub(crate) fn test(clustered: bool, namespace: &str) -> Self {
        let url = if clustered {
            "redis-cluster://127.0.0.1:7000"
        } else {
            "redis://127.0.0.1:6379"
        };

        Self {
            urls: vec![url.parse().unwrap()],
            username: None,
            password: None,
            timeout: Duration::from_millis(500),
            ttl: Some(Duration::from_secs(300)),
            namespace: Some(namespace.to_string()),
            tls: None,
            required_to_start: true,
            reset_ttl: false,
            pool_size: 1,
            metrics_interval: Duration::from_secs(1),
        }
    }
}
