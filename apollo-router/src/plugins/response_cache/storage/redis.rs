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
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio::task::JoinSet;
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
        let pipeline = self.writer_storage.client().pipeline();
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
        let storage = self.clone();

        // spawn a task that reads from cache_tag_rx and uses `zremrangebyscore` on each cache tag
        tokio::spawn(async move {
            while let Some(cache_tag) = cache_tag_rx.recv().await {
                // NB: `cache_tag` already includes namespace
                let cutoff = now_epoch_seconds() - 1;

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
    fn timeout_duration(&self) -> Duration {
        self.timeout
    }

    async fn _insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
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
                    document.cache_key.clone(),
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
                cache_control: document.cache_control,
            };
            let _: () = pipeline
                .set::<(), _, _>(
                    document.cache_key,
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

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl Storage {
    async fn mocked(
        config: &Config,
        is_cluster: bool,
        reader_mock: std::sync::Arc<dyn fred::mocks::Mocks>,
        writer_mock: std::sync::Arc<dyn fred::mocks::Mocks>,
    ) -> Result<Storage, BoxError> {
        let reader_storage = RedisCacheStorage::from_mocks_and_config(
            reader_mock,
            config.clone(),
            "response-cache-reader",
            is_cluster,
        )
        .await?;
        let writer_storage = RedisCacheStorage::from_mocks_and_config(
            writer_mock,
            config.clone(),
            "response-cache-writer",
            is_cluster,
        )
        .await?;
        Self::from_storage(reader_storage, writer_storage, config.timeout).await
    }

    async fn all_keys_in_namespace(&self) -> Result<Vec<String>, BoxError> {
        use fred::types::scan::Scanner;
        use tokio_stream::StreamExt;

        let mut scan_stream = self
            .reader_storage
            .scan_with_namespaced_results(String::from("*"), None);
        let mut keys = Vec::default();
        while let Some(result) = scan_stream.next().await {
            if let Some(page_keys) = result?.take_results() {
                let mut str_keys: Vec<String> = page_keys
                    .into_iter()
                    .map(|k| k.into_string().unwrap())
                    .collect();
                keys.append(&mut str_keys);
            }
        }

        Ok(keys)
    }

    async fn ttl(&self, key: &str) -> StorageResult<i64> {
        Ok(self.reader_storage.client().ttl(key).await?)
    }

    async fn expire_time(&self, key: &str) -> StorageResult<i64> {
        Ok(self.reader_storage.client().expire_time(key).await?)
    }

    async fn zscore(&self, sorted_set_key: &str, member: &str) -> Result<i64, BoxError> {
        let score: String = self
            .reader_storage
            .client()
            .zscore(sorted_set_key, member)
            .await?;
        Ok(score.parse()?)
    }

    async fn zcard(&self, sorted_set_key: &str) -> StorageResult<u64> {
        let cardinality = self.reader_storage.client().zcard(sorted_set_key).await?;
        Ok(cardinality)
    }

    async fn zexists(&self, sorted_set_key: &str, member: &str) -> StorageResult<bool> {
        let score: Option<String> = self
            .reader_storage
            .client()
            .zscore(sorted_set_key, member)
            .await?;
        Ok(score.is_some())
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
pub(crate) fn default_redis_cache_config() -> Config {
    Config {
        urls: vec!["redis://127.0.0.1:6379".parse().unwrap()],
        username: None,
        password: None,
        timeout: Duration::from_millis(500),
        ttl: Some(Duration::from_secs(300)),
        namespace: None,
        tls: None,
        required_to_start: true,
        reset_ttl: false,
        pool_size: 1,
        metrics_interval: Duration::from_secs(1),
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use insta::assert_debug_snapshot;
    use itertools::Itertools;
    use tower::BoxError;

    use super::Config;
    use super::Storage;
    use super::default_redis_cache_config;
    use crate::plugins::response_cache::cache_control::now_epoch_seconds;
    use crate::plugins::response_cache::storage::CacheStorage;
    use crate::plugins::response_cache::storage::Document;

    const SUBGRAPH_NAME: &str = "test";

    fn redis_config(namespace: &str, clustered: bool) -> Config {
        let url = if clustered {
            "redis-cluster://127.0.0.1:7000"
        } else {
            "redis://127.0.0.1:6379"
        };

        Config {
            urls: vec![url.parse().unwrap()],
            namespace: Some(namespace.to_string()),
            ..default_redis_cache_config()
        }
    }

    fn common_document() -> Document {
        Document {
            cache_key: "cache_key".to_string(),
            data: Default::default(),
            cache_control: Default::default(),
            invalidation_keys: vec!["invalidate".to_string()],
            expire: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    #[rstest::rstest]
    async fn test_invalidation_key_permutations(
        #[values(None, Some("test"))] namespace: Option<&str>,
        #[values(vec![], vec!["invalidation_key"], vec!["invalidation_key1", "invalidation_key2", "invalidation_key3"])]
        invalidation_keys: Vec<&str>,
    ) {
        // Set up insta snapshot to support test parameterization
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(format!(
            "input____{}____{}",
            namespace.unwrap_or("None"),
            invalidation_keys.iter().join("__")
        ));
        let _guard = settings.bind_to_scope();

        let mock_storage = Arc::new(fred::mocks::Echo);
        let config = Config {
            namespace: namespace.map(ToString::to_string),
            ..redis_config("", false)
        };
        let storage = Storage::mocked(&config, false, mock_storage.clone(), mock_storage.clone())
            .await
            .expect("could not build storage");

        let invalidation_keys: Vec<String> = invalidation_keys
            .into_iter()
            .map(ToString::to_string)
            .collect();

        let mut cache_tags = storage.namespaced_cache_tags(&invalidation_keys, "products");
        cache_tags.sort();
        assert_debug_snapshot!(cache_tags);
    }

    /// Tests that validate the following TTL behaviors:
    /// * a document's TTL must be shorter than the TTL of all its related cache tags
    /// * a document's TTL will always be less than or equal to its score in all its related cache tags
    /// * only expired keys will be removed via the cache maintenance
    mod ttl_guarantees {
        use std::collections::HashMap;
        use std::time::Duration;

        use itertools::Itertools;
        use tower::BoxError;

        use super::SUBGRAPH_NAME;
        use super::common_document;
        use super::redis_config;
        use crate::plugins::response_cache::storage::CacheStorage;
        use crate::plugins::response_cache::storage::Document;
        use crate::plugins::response_cache::storage::redis::Storage;

        #[tokio::test]
        #[rstest::rstest]
        async fn single_document(#[values(true, false)] clustered: bool) -> Result<(), BoxError> {
            let config = redis_config("single_document", clustered);
            let storage = Storage::new(&config).await?;

            // every element of this namespace must have a TTL associated with it, and the TTL of the
            // cache keys must be greater than the TTL of the document
            let document = common_document();
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            let document_key = storage.make_key(document.cache_key.clone());
            let expected_cache_tag_keys =
                storage.namespaced_cache_tags(&document.invalidation_keys, SUBGRAPH_NAME);

            // iterate over all the keys in the namespace and make sure we have everything we'd expect
            let keys = storage.all_keys_in_namespace().await?;
            assert!(keys.contains(&document_key));
            for key in &expected_cache_tag_keys {
                assert!(keys.contains(key), "missing {key}");
            }
            assert_eq!(keys.len(), 4);

            // extract the TTL for each key. the TTL for the document must be less than the TTL for each
            // of the invalidation keys.
            let document_ttl = storage.ttl(&document_key).await?;
            assert!(document_ttl > 0);

            for cache_tag_key in &expected_cache_tag_keys {
                let cache_tag_ttl = storage.ttl(cache_tag_key).await?;
                assert!(cache_tag_ttl > 0, "{cache_tag_key}");
                assert!(document_ttl < cache_tag_ttl, "{cache_tag_key}")
            }

            // extract the expiry time for the document key. it should match the sorted set score in each
            // of the cache tags.
            let document_expire_time = storage.expire_time(&document_key).await?;
            assert!(document_expire_time > 0);

            for cache_tag_key in &expected_cache_tag_keys {
                let document_score = storage.zscore(cache_tag_key, &document_key).await?;
                assert_eq!(document_expire_time, document_score);
            }

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn multiple_documents(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let config = redis_config("multiple_documents", clustered);
            let storage = Storage::new(&config).await?;

            // set up two documents with a shared key and different TTLs
            let documents = vec![
                Document {
                    cache_key: "cache_key1".to_string(),
                    invalidation_keys: vec![
                        "invalidation".to_string(),
                        "invalidation1".to_string(),
                    ],
                    expire: Duration::from_secs(30),
                    ..common_document()
                },
                Document {
                    cache_key: "cache_key2".to_string(),
                    invalidation_keys: vec![
                        "invalidation".to_string(),
                        "invalidation2".to_string(),
                    ],
                    expire: Duration::from_secs(60),
                    ..common_document()
                },
            ];
            storage
                .insert_in_batch(documents.clone(), SUBGRAPH_NAME)
                .await?;

            // based on these documents, we expect:
            // * subgraph cache-tag TTL ~60s
            // * `invalidation` cache-tag TTL ~60s
            // * `invalidation1` cache-tag TTL ~30s
            // * `invalidation2` cache-tag TTL ~60s
            // since those are the maximums observed

            let mut expected_document_keys = Vec::new();
            let mut expected_cache_tag_keys = Vec::new();
            for document in &documents {
                expected_document_keys.push(storage.make_key(&document.cache_key));
                expected_cache_tag_keys.push(
                    storage.namespaced_cache_tags(&document.invalidation_keys, SUBGRAPH_NAME),
                );
            }

            let all_expected_cache_tag_keys: Vec<String> = expected_cache_tag_keys
                .iter()
                .flatten()
                .cloned()
                .unique()
                .collect();

            // we should have a few shared keys
            assert!(
                all_expected_cache_tag_keys.len()
                    < expected_cache_tag_keys.iter().map(|keys| keys.len()).sum()
            );

            // iterate over all the keys in the namespace and make sure we have everything we'd expect
            let keys = storage.all_keys_in_namespace().await?;
            for expected_document_key in &expected_document_keys {
                assert!(keys.contains(expected_document_key));
            }
            for expected_cache_tag_key in &all_expected_cache_tag_keys {
                assert!(keys.contains(expected_cache_tag_key));
            }
            assert_eq!(keys.len(), 9);

            // extract all TTLs
            let mut ttls: HashMap<String, i64> = HashMap::default();
            for key in &keys {
                let ttl = storage.ttl(key).await?;
                assert!(ttl > 0);
                ttls.insert(key.clone(), ttl);
            }

            // for each document, make sure that its cache tags have a TTL greater than its own
            for (index, document) in documents.iter().enumerate() {
                let document_key = &expected_document_keys[index];
                let cache_tag_keys = &expected_cache_tag_keys[index];

                let document_ttl = ttls.get(document_key).unwrap();

                // the document TTL should be close to the expiry time on the document (within some range
                // of acceptable redis latency - 10s for now)
                assert!(document.expire.as_secs() as i64 - *document_ttl < 10);

                for cache_tag_key in cache_tag_keys {
                    let cache_tag_ttl = ttls.get(cache_tag_key).unwrap();
                    assert!(document_ttl < cache_tag_ttl);
                }
            }

            // for each document, make sure the expiry time matches its score in each cache tag set
            for index in 0..documents.len() {
                let document_key = &expected_document_keys[index];
                let cache_tag_keys = &expected_cache_tag_keys[index];

                let document_expire_time = storage.expire_time(document_key).await?;
                assert!(document_expire_time > 0);

                for cache_tag_key in cache_tag_keys {
                    let document_score = storage.zscore(cache_tag_key, document_key).await?;
                    assert_eq!(document_expire_time, document_score);
                }
            }

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn cache_tag_ttl_will_only_increase(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let config = redis_config("cache_tag_ttl_will_only_increase", clustered);
            let storage = Storage::new(&config).await?;

            let document = Document {
                cache_key: "cache_key1".to_string(),
                expire: Duration::from_secs(60),
                ..common_document()
            };
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            let keys = storage.all_keys_in_namespace().await?;

            // save current expiry times
            let mut expire_times: HashMap<String, i64> = HashMap::default();
            for key in &keys {
                let expire_time = storage.expire_time(key).await?;
                assert!(expire_time > 0);
                expire_times.insert(key.clone(), expire_time);
            }

            // add another document with a very short expiry time but the same cache tags
            let document = Document {
                cache_key: "cache_key2".to_string(),
                expire: Duration::from_secs(1),
                ..common_document()
            };
            storage.insert(document, SUBGRAPH_NAME).await?;

            // fetch new expiry times; they should be the same
            for key in keys {
                let new_expire_time = storage.expire_time(&key).await?;
                assert!(new_expire_time > 0);
                assert_eq!(*expire_times.get(&key).unwrap(), new_expire_time);
            }

            Ok(())
        }

        /// When re-inserting the same key with a lower TTL, the score in the sorted set will not
        /// decrease.
        ///
        /// This might seem strange, but it's a defensive mechanism in case the insert fails midway
        /// through - we don't want to lower the cache tag score only to not change the TTL on the key.
        #[tokio::test]
        #[rstest::rstest]
        async fn cache_tag_score_will_not_decrease(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let config = redis_config("cache_tag_score_will_not_decrease", clustered);
            let storage = Storage::new(&config).await?;

            let document = Document {
                expire: Duration::from_secs(60),
                data: serde_json_bytes::Value::Number(1.into()),
                ..common_document()
            };
            let document_key = storage.make_key(document.cache_key.clone());
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was stored
            let stored_data = storage.get(&common_document().cache_key).await?;
            assert_eq!(stored_data.data, document.data);

            let keys = storage.namespaced_cache_tags(&document.invalidation_keys, SUBGRAPH_NAME);

            // save current scores
            let mut scores: HashMap<String, i64> = HashMap::default();
            let mut expire_times: HashMap<String, i64> = HashMap::default();
            for key in &keys {
                let score = storage.zscore(key, &document_key).await?;
                assert!(score > 0);
                scores.insert(key.clone(), score);

                let expire_time = storage.expire_time(key).await?;
                assert!(expire_time > 0);
                expire_times.insert(key.clone(), expire_time);
            }

            // update the document with new data and a shorter TTL
            let document = Document {
                expire: Duration::from_secs(10),
                data: serde_json_bytes::Value::Number(2.into()),
                ..common_document()
            };
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was updated
            let stored_data = storage.get(&document.cache_key).await?;
            assert_eq!(stored_data.data, document.data);

            // the TTL on the document should be aligned with the new document expiry time
            let ttl = storage.ttl(&document_key).await?;
            assert!(ttl <= document.expire.as_secs() as i64);

            // however, the TTL on the cache tags and the score in the cache tags will be the same
            for key in keys {
                let score = storage.zscore(&key, &document_key).await?;
                assert!(score > 0);
                assert_eq!(*scores.get(&key).unwrap(), score);

                let expire_time = storage.expire_time(&key).await?;
                assert!(expire_time > 0);
                assert_eq!(*expire_times.get(&key).unwrap(), expire_time);
            }

            Ok(())
        }

        /// When re-inserting the same key with a later expiry time, the score in the sorted set will
        /// increase.
        #[tokio::test]
        #[rstest::rstest]
        async fn cache_tag_score_will_increase(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let config = redis_config("cache_tag_score_will_increase", clustered);
            let storage = Storage::new(&config).await?;

            let document = Document {
                expire: Duration::from_secs(60),
                data: serde_json_bytes::Value::Number(1.into()),
                ..common_document()
            };
            let document_key = storage.make_key(document.cache_key.clone());
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was stored
            let stored_data = storage.get(&common_document().cache_key).await?;
            assert_eq!(stored_data.data, document.data);

            let keys = storage.namespaced_cache_tags(&document.invalidation_keys, SUBGRAPH_NAME);

            // update the document with new data and a longer TTL
            let old_ttl = document.expire;
            let document = Document {
                expire: old_ttl * 2,
                data: serde_json_bytes::Value::Number(2.into()),
                ..common_document()
            };
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was updated
            let stored_data = storage.get(&document.cache_key).await?;
            assert_eq!(stored_data.data, document.data);

            // the TTL on the document should be aligned with the new document expiry time
            let ttl = storage.ttl(&document_key).await?;
            assert!(ttl <= document.expire.as_secs() as i64);
            assert!(ttl > old_ttl.as_secs() as i64);

            let doc_expire_time = storage.expire_time(&document_key).await?;

            // the TTL on the cache tags and the score in the cache tags should have also increased
            for key in keys {
                let score = storage.zscore(&key, &document_key).await?;
                assert!(doc_expire_time <= score);

                let expire_time = storage.expire_time(&key).await?;
                assert!(doc_expire_time < expire_time);
            }

            Ok(())
        }
    }

    /// Tests that ensure that if a key's cache tag cannot be updated, the key will not be updated.
    mod cache_tag_insert_failure_should_abort_key_insertion {
        use std::sync::Arc;
        use std::time::Duration;
        use std::time::Instant;

        use fred::error::Error;
        use fred::error::ErrorKind;
        use fred::interfaces::KeysInterface;
        use fred::mocks::MockCommand;
        use fred::mocks::Mocks;
        use fred::prelude::Expiration;
        use fred::prelude::Value;
        use parking_lot::RwLock;
        use tower::BoxError;

        use super::SUBGRAPH_NAME;
        use super::common_document;
        use super::redis_config;
        use crate::plugins::response_cache::storage::CacheStorage;
        use crate::plugins::response_cache::storage::Document;
        use crate::plugins::response_cache::storage::redis::Storage;

        /// Trigger failure by pre-setting the cache tag to an invalid type.
        #[tokio::test]
        #[rstest::rstest]
        async fn type_failure(#[values(true, false)] clustered: bool) -> Result<(), BoxError> {
            let config = redis_config("type_failure", clustered);
            let storage = Storage::new(&config).await?;

            let document = common_document();
            let document_key = storage.make_key(document.cache_key.clone());
            let cache_tag_keys =
                storage.namespaced_cache_tags(&document.invalidation_keys, SUBGRAPH_NAME);

            let insert_invalid_cache_tag = |key: String| async {
                let expiration = config.ttl.map(|ttl| Expiration::EX(ttl.as_secs() as i64));
                let _: () = storage
                    .writer_storage
                    .client()
                    .set(key, 1, expiration, None, false)
                    .await?;
                Ok::<(), BoxError>(())
            };
            let inserted_data = |key: String| async {
                let exists = storage.reader_storage.client().exists(key).await?;
                Ok::<bool, BoxError>(exists)
            };

            // try performing the insert with one of the cache_tag_keys set to a string so that the ZADD
            // is guaranteed to fail.
            // NB: we do this for each key because fred might report a failure at the beginning of a pipeline
            // differently than a failure at the end.
            for key in cache_tag_keys {
                storage.truncate_namespace().await?;
                insert_invalid_cache_tag(key.clone()).await?;

                let result = storage.insert(document.clone(), SUBGRAPH_NAME).await;
                result.expect_err(&format!(
                    "cache tag {key} should have caused insertion failure"
                ));

                assert!(!inserted_data(document_key.clone()).await?);
            }

            // this should also be true if inserting multiple documents, even if only one of the
            // documents' cache tags couldn't be updated.
            let documents = vec![
                Document {
                    cache_key: "cache_key1".to_string(),
                    invalidation_keys: vec![],
                    ..common_document()
                },
                Document {
                    cache_key: "cache_key2".to_string(),
                    invalidation_keys: vec!["invalidate".to_string()],
                    ..common_document()
                },
            ];

            let cache_tag_keys =
                storage.namespaced_cache_tags(&documents[1].invalidation_keys, SUBGRAPH_NAME);
            for key in cache_tag_keys {
                storage.truncate_namespace().await?;
                insert_invalid_cache_tag(key.clone()).await?;

                storage
                    .insert_in_batch(documents.clone(), SUBGRAPH_NAME)
                    .await
                    .expect_err(&format!(
                        "cache tag {key} should have caused insertion failure"
                    ));

                for document in &documents {
                    assert!(!inserted_data(storage.make_key(document.cache_key.clone())).await?);
                }
            }

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn timeout_failure(#[values(true, false)] clustered: bool) -> Result<(), BoxError> {
            use crate::plugins::response_cache::storage::Error as StorageError;

            // Mock the Redis connection to be able to simulate a timeout error coming from within
            // the `fred` client
            #[derive(Default, Debug, Clone)]
            struct MockStorage(Arc<RwLock<Vec<MockCommand>>>);
            impl Mocks for MockStorage {
                fn process_command(&self, command: MockCommand) -> Result<Value, Error> {
                    self.0.write().push(command);
                    Err(Error::new(ErrorKind::Timeout, "timeout"))
                }
            }

            let config = redis_config("timeout_failure", clustered);
            let mock_redis = Arc::new(MockStorage::default());
            let storage =
                Storage::mocked(&config, false, mock_redis.clone(), mock_redis.clone()).await?;

            let document = common_document();
            let document_key = Value::from(storage.make_key(document.cache_key.clone()));

            let result = storage.insert(document, SUBGRAPH_NAME).await;
            let error = result.expect_err("should have timed out via redis");
            assert!(matches!(error, StorageError::Redis(e) if e.details() == "timeout"));

            // make sure the insert function did not try to operate on the document key
            for command in mock_redis.0.read().iter() {
                if command.cmd.contains("SET") && command.args.contains(&document_key) {
                    panic!("Command {command:?} set the document key");
                }
            }

            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        #[rstest::rstest]
        async fn no_response_failure(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            use crate::plugins::response_cache::storage::Error as StorageError;

            // Mock the Redis connection to simulate the `fred` client not returning, despite the
            // timeout value being configured
            #[derive(Default, Debug, Clone)]
            struct MockStorage(Arc<RwLock<Vec<MockCommand>>>);
            impl Mocks for MockStorage {
                fn process_command(&self, command: MockCommand) -> Result<Value, Error> {
                    self.0.write().push(command);

                    // NB: need to sleep in an async task so that the task can be aborted by
                    // `CacheStorage::insert`
                    let runtime = tokio::runtime::Handle::try_current().unwrap();
                    let handle = runtime.spawn(async {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    });
                    while !handle.is_finished() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Ok(Value::Null)
                }
            }

            let config = redis_config("no_response_failure", clustered);
            let mock_redis = Arc::new(MockStorage::default());
            let storage =
                Storage::mocked(&config, false, mock_redis.clone(), mock_redis.clone()).await?;

            let document = common_document();
            let document_key = Value::from(storage.make_key(document.cache_key.clone()));

            let now = Instant::now();
            let result = storage.insert(document, SUBGRAPH_NAME).await;
            let error = result.expect_err("should have timed out via the tokio::timeout wrapper");
            assert!(
                matches!(error, StorageError::Timeout)
                    || matches!(error, StorageError::Redis(e) if e.kind() == &ErrorKind::Timeout)
            );

            // elapsed time should be close to the configured timeout
            let elapsed = now.elapsed();
            assert!(elapsed < 2 * config.timeout);

            // make sure the insert function did not try to operate on the document key
            for command in mock_redis.0.read().iter() {
                if command.cmd.contains("SET") && command.args.contains(&document_key) {
                    panic!("Command {command:?} set the document key");
                }
            }

            Ok(())
        }
    }

    #[tokio::test]
    #[rstest::rstest]
    async fn maintenance_removes_expired_data(
        #[values(true, false)] clustered: bool,
    ) -> Result<(), BoxError> {
        let config = redis_config("maintenance_removes_expired_data", clustered);
        let storage = Storage::new(&config).await?;

        // set up two documents with a shared key and different TTLs
        let documents = vec![
            Document {
                cache_key: "cache_key1".to_string(),
                expire: Duration::from_secs(2),
                ..common_document()
            },
            Document {
                cache_key: "cache_key2".to_string(),
                expire: Duration::from_secs(60),
                ..common_document()
            },
            Document {
                cache_key: "cache_key3".to_string(),
                expire: Duration::from_secs(60),
                ..common_document()
            },
        ];
        storage
            .insert_in_batch(documents.clone(), SUBGRAPH_NAME)
            .await?;

        // ensure that we have three elements in the 'whole-subgraph' invalidation key
        let invalidation_key = storage
            .namespaced_cache_tags(&vec![], SUBGRAPH_NAME)
            .remove(0);
        assert_eq!(storage.zcard(&invalidation_key).await?, 3);

        let doc_key1 = storage.make_key("cache_key1");
        let doc_key2 = storage.make_key("cache_key2");
        let doc_key3 = storage.make_key("cache_key3");
        for key in [&doc_key1, &doc_key2, &doc_key3] {
            assert!(storage.zexists(&invalidation_key, key).await?);
        }

        // manually trigger maintenance with a time in the future, in between the expiry times of doc1
        // and docs 2 and 3. therefore, we should remove `cache_key1` and leave `cache_key2` and `cache_key3`
        let cutoff = now_epoch_seconds() + 10;
        assert!(storage.zscore(&invalidation_key, &doc_key1).await? < cutoff as i64);
        let removed_keys = storage
            .remove_keys_from_cache_tag_by_cutoff(invalidation_key.clone(), cutoff as f64)
            .await?;
        assert_eq!(removed_keys, 1);

        // now we should have two elements in the 'whole-subgraph' invalidation key
        assert_eq!(storage.zcard(&invalidation_key).await?, 2);
        assert!(!storage.zexists(&invalidation_key, &doc_key1).await?);
        assert!(storage.zexists(&invalidation_key, &doc_key2).await?);
        assert!(storage.zexists(&invalidation_key, &doc_key3).await?);

        // manually trigger maintenance with the time set way in the future
        let cutoff = now_epoch_seconds() + 1000;
        let removed_keys = storage
            .remove_keys_from_cache_tag_by_cutoff(invalidation_key.clone(), cutoff as f64)
            .await?;
        assert_eq!(removed_keys, 2);

        // now we should have zero elements in the 'whole-subgraph' invalidation key
        assert_eq!(storage.zcard(&invalidation_key).await?, 0);
        for key in [&doc_key1, &doc_key2, &doc_key3] {
            assert!(!storage.zexists(&invalidation_key, key).await?);
        }

        Ok(())
    }

    // TODO: invalidation
}
