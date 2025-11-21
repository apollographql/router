use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use fred::interfaces::ClientLike;
use fred::interfaces::KeysInterface;
use fred::interfaces::SortedSetsInterface;
use fred::prelude::Options;
use fred::types::Expiration;
use fred::types::ExpireOptions;
use fred::types::Value;
use fred::types::sorted_sets::Ordering;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::time::FutureExt;
use tower::BoxError;

use super::CacheEntry;
use super::CacheStorage;
use super::Document;
use super::StorageResult;
use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::cache::storage::KeyType;
use crate::cache::storage::ValueType;
use crate::plugins::response_cache::cache_control::CacheControl;
use crate::plugins::response_cache::metrics::record_maintenance_duration;
use crate::plugins::response_cache::metrics::record_maintenance_error;
use crate::plugins::response_cache::metrics::record_maintenance_queue_error;
use crate::plugins::response_cache::metrics::record_maintenance_success;

pub(crate) type Config = super::config::Config;

#[derive(Deserialize, Debug, Clone, Serialize)]
struct CacheValue {
    data: serde_json_bytes::Value,
    cache_control: CacheControl,
    // Only set in debug mode
    cache_tags: Option<HashSet<String>>,
}

impl ValueType for CacheValue {}

impl From<(&str, CacheValue)> for CacheEntry {
    fn from((cache_key, cache_value): (&str, CacheValue)) -> Self {
        CacheEntry {
            key: cache_key.to_string(),
            data: cache_value.data,
            control: cache_value.cache_control,
            cache_tags: cache_value.cache_tags,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Storage {
    storage: RedisCacheStorage,
    cache_tag_tx: mpsc::Sender<String>,
    fetch_timeout: Duration,
    insert_timeout: Duration,
    invalidate_timeout: Duration,
    maintenance_timeout: Duration,
}

impl Storage {
    pub(crate) async fn new(
        config: &Config,
        drop_rx: broadcast::Receiver<()>,
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

        let storage = RedisCacheStorage::new(config.into(), "response-cache").await?;
        let (cache_tag_tx, cache_tag_rx) = mpsc::channel(1000);
        let s = Self {
            storage,
            cache_tag_tx,
            fetch_timeout: config.fetch_timeout,
            insert_timeout: config.insert_timeout,
            invalidate_timeout: config.invalidate_timeout,
            maintenance_timeout: config.maintenance_timeout,
        };
        s.perform_periodic_maintenance(cache_tag_rx, drop_rx).await;
        Ok(s)
    }

    fn make_key<K: KeyType>(&self, key: K) -> String {
        self.storage.make_key(RedisKey(key))
    }

    async fn invalidate_keys(&self, invalidation_keys: Vec<String>) -> StorageResult<u64> {
        let options = Options {
            timeout: Some(self.invalidate_timeout()),
            ..Options::default()
        };
        let pipeline = self.storage.pipeline().with_options(&options);
        for invalidation_key in &invalidation_keys {
            let invalidation_key = self.make_key(format!("cache-tag:{invalidation_key}"));
            self.send_to_maintenance_queue(invalidation_key.clone());
            let _: () = pipeline
                .zrange(invalidation_key.clone(), 0, -1, None, false, None, false)
                .await?;
        }

        let results: Vec<Vec<String>> = pipeline.all().await?;
        let all_keys: HashSet<String> = results.into_iter().flatten().collect();
        if all_keys.is_empty() {
            return Ok(0);
        }

        let keys = all_keys.into_iter().map(fred::types::Key::from);
        let deleted = self
            .storage
            .delete_from_scan_result_with_options(keys, options)
            .await?;

        // NOTE: we don't delete elements from the cache tag sorted sets. if we did, we would likely
        // encounter a race condition - if another router inserted a value associated with this cache
        // tag between when we run the `zrange` and the `delete`.
        // it's safer to just rely on the TTL-based cleanup.
        Ok(deleted as u64)
    }

    fn send_to_maintenance_queue(&self, cache_tag_key: String) {
        if let Err(err) = self.cache_tag_tx.try_send(cache_tag_key) {
            record_maintenance_queue_error(&err);
        }
    }

    pub(crate) async fn perform_periodic_maintenance(
        &self,
        mut cache_tag_rx: mpsc::Receiver<String>,
        mut drop_rx: broadcast::Receiver<()>,
    ) {
        let storage = self.clone();

        // spawn a task that reads from cache_tag_rx and uses `zremrangebyscore` on each cache tag
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = drop_rx.recv() => break,
                    Some(cache_tag) = cache_tag_rx.recv() => storage.perform_maintenance_on_cache_tag(cache_tag).await
                }
            }
        });
    }

    async fn perform_maintenance_on_cache_tag(&self, cache_tag: String) {
        // NB: `cache_tag` already includes namespace
        let cutoff = now() - 1;

        let now = Instant::now();
        let removed_items_result = super::flatten_storage_error(
            self.remove_keys_from_cache_tag_by_cutoff(cache_tag, cutoff as f64)
                .timeout(self.maintenance_timeout())
                .await,
        );
        record_maintenance_duration(now.elapsed());

        match removed_items_result {
            Ok(removed_items) => record_maintenance_success(removed_items),
            Err(err) => record_maintenance_error(&err),
        }
    }

    async fn remove_keys_from_cache_tag_by_cutoff(
        &self,
        cache_tag_key: String,
        cutoff_time: f64,
    ) -> StorageResult<u64> {
        // Returns number of items removed
        let options = Options {
            timeout: Some(self.maintenance_timeout()),
            ..Options::default()
        };
        Ok(self
            .storage
            .client()
            .with_options(&options)
            .zremrangebyscore(&cache_tag_key, f64::NEG_INFINITY, cutoff_time)
            .await?)
    }

    /// Create a list of the cache tags that describe this document, with associated namespaces.
    ///
    /// For a given subgraph `s` and invalidation keys `i1`, `i2`, ..., we need to store the
    /// following subgraph-invalidation-key permutations:
    /// * `subgraph-{s}` (whole subgraph)
    /// * `subgraph-{s}:key-{i1}`, `subgraph-{s}:key-{i2}`, ... (invalidation key per subgraph)
    ///
    /// These are then turned into redis keys by adding the namespace and a `cache-tag:` prefix, ie:
    /// * `{namespace}:cache-tag:subgraph-{s}`
    /// * `{namespace}:cache-tag:subgraph-{s}:key-{i1}`, ...
    fn namespaced_cache_tags(
        &self,
        document_invalidation_keys: &[String],
        subgraph_name: &str,
    ) -> Vec<String> {
        let mut cache_tags = Vec::with_capacity(1 + document_invalidation_keys.len());
        cache_tags.push(format!("subgraph-{subgraph_name}"));
        for invalidation_key in document_invalidation_keys {
            cache_tags.push(format!("subgraph-{subgraph_name}:key-{invalidation_key}"));
        }

        for cache_tag in cache_tags.iter_mut() {
            *cache_tag = self.make_key(format!("cache-tag:{cache_tag}"));
        }

        cache_tags
    }

    fn maintenance_timeout(&self) -> Duration {
        self.maintenance_timeout
    }
}

impl CacheStorage for Storage {
    fn insert_timeout(&self) -> Duration {
        self.insert_timeout
    }

    fn fetch_timeout(&self) -> Duration {
        self.fetch_timeout
    }

    fn invalidate_timeout(&self) -> Duration {
        self.invalidate_timeout
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
        //   1 - update keys, cache tags to include namespace so that we don't have to do it in each phase
        //   2 - update each cache tag with new keys
        //   3 - update each key
        // a failure in any phase will cause the function to return, which prevents invalid states

        let now = now();

        // Only useful for caching debugger, it will only contains entries if the doc is set to debug
        let mut original_cache_tags = Vec::with_capacity(batch_docs.len());
        // phase 1
        for document in &mut batch_docs {
            document.key = self.make_key(&document.key);
            if document.debug {
                original_cache_tags.push(document.invalidation_keys.clone());
            } else {
                original_cache_tags.push(Vec::new());
            }
            document.invalidation_keys =
                self.namespaced_cache_tags(&document.invalidation_keys, subgraph_name);
        }

        // phase 2
        let num_cache_tags_estimate = 2 * batch_docs.len();
        let mut cache_tags_to_pcks: HashMap<String, Vec<(f64, String)>> =
            HashMap::with_capacity(num_cache_tags_estimate);
        for document in &mut batch_docs {
            for cache_tag_key in document.invalidation_keys.drain(..) {
                let cache_tag_value = (
                    (now + document.expire.as_secs()) as f64,
                    document.key.clone(),
                );
                // NB: performance concerns with `entry` API
                if let Some(entry) = cache_tags_to_pcks.get_mut(&cache_tag_key) {
                    entry.push(cache_tag_value);
                } else {
                    cache_tags_to_pcks.insert(cache_tag_key, vec![cache_tag_value]);
                }
            }
        }

        let options = Options {
            timeout: Some(self.insert_timeout()),
            ..Options::default()
        };
        let pipeline = self.storage.pipeline().with_options(&options);
        for (cache_tag_key, elements) in cache_tags_to_pcks.into_iter() {
            self.send_to_maintenance_queue(cache_tag_key.clone());

            // NB: expiry time being max + 1 is important! if you use a volatile TTL eviction policy,
            // Redis will evict the keys with the shortest TTLs - we have to make sure that the cache
            // tag will outlive any of the keys it refers to.
            let max_expiry_time = elements
                .iter()
                .map(|(exp_time, _)| *exp_time)
                .fold(now as f64, f64::max);
            let cache_tag_expiry_time = max_expiry_time as i64 + 1;

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
                    .expire_at(cache_tag_key.clone(), cache_tag_expiry_time, Some(exp_opt))
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
        let pipeline = self.storage.pipeline().with_options(&options);
        for (document, cache_tags) in batch_docs.into_iter().zip(original_cache_tags.into_iter()) {
            let value = CacheValue {
                data: document.data,
                cache_control: document.control,
                cache_tags: document.debug.then(|| cache_tags.into_iter().collect()),
            };
            let _: () = pipeline
                .set::<(), _, _>(
                    document.key,
                    &serde_json::to_string(&value)?,
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
        // NB: don't need `make_key` for `get` - the storage layer already runs it
        let options = Options {
            timeout: Some(self.fetch_timeout()),
            ..Options::default()
        };
        let value: RedisValue<CacheValue> = self
            .storage
            .get_with_options(RedisKey(cache_key), options)
            .await?;
        Ok(CacheEntry::from((cache_key, value.0)))
    }

    async fn internal_fetch_multiple(
        &self,
        cache_keys: &[&str],
    ) -> StorageResult<Vec<Option<CacheEntry>>> {
        let keys: Vec<RedisKey<String>> = cache_keys
            .iter()
            .map(|key| RedisKey(key.to_string()))
            .collect();
        let options = Options {
            timeout: Some(self.fetch_timeout()),
            ..Options::default()
        };
        let values: Vec<Option<RedisValue<CacheValue>>> =
            self.storage.get_multiple_with_options(keys, options).await;

        let entries = values
            .into_iter()
            .zip(cache_keys)
            .map(|(opt_value, cache_key)| {
                opt_value.map(|value| CacheEntry::from((*cache_key, value.0)))
            })
            .collect();

        Ok(entries)
    }

    async fn internal_invalidate_by_subgraph(&self, subgraph_name: &str) -> StorageResult<u64> {
        self.invalidate_keys(vec![format!("subgraph-{subgraph_name}")])
            .await
    }

    async fn internal_invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> StorageResult<HashMap<String, u64>> {
        let mut join_set = JoinSet::default();
        let num_subgraphs = subgraph_names.len();

        for subgraph_name in subgraph_names {
            let keys: Vec<String> = invalidation_keys
                .iter()
                .map(|invalidation_key| format!("subgraph-{subgraph_name}:key-{invalidation_key}"))
                .collect();
            let storage = self.clone();
            join_set.spawn(async move { (subgraph_name, storage.invalidate_keys(keys).await) });
        }

        let mut counts = HashMap::with_capacity(num_subgraphs);
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
        self.storage.truncate_namespace().await?;
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
impl Storage {
    async fn mocked(
        config: &Config,
        is_cluster: bool,
        mock_storage: std::sync::Arc<dyn fred::mocks::Mocks>,
        drop_rx: broadcast::Receiver<()>,
    ) -> Result<Storage, BoxError> {
        let storage = RedisCacheStorage::from_mocks_and_config(
            mock_storage,
            config.into(),
            "response-cache",
            is_cluster,
        )
        .await?;
        let (cache_tag_tx, cache_tag_rx) = mpsc::channel(100);
        let s = Self {
            storage,
            cache_tag_tx,
            fetch_timeout: config.fetch_timeout,
            insert_timeout: config.insert_timeout,
            invalidate_timeout: config.invalidate_timeout,
            maintenance_timeout: config.maintenance_timeout,
        };
        s.perform_periodic_maintenance(cache_tag_rx, drop_rx).await;
        Ok(s)
    }

    async fn all_keys_in_namespace(&self) -> Result<Vec<String>, BoxError> {
        use fred::types::scan::Scanner;
        use tokio_stream::StreamExt;

        let mut scan_stream = self
            .storage
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
        Ok(self.storage.client().ttl(key).await?)
    }

    async fn expire_time(&self, key: &str) -> StorageResult<i64> {
        Ok(self.storage.client().expire_time(key).await?)
    }

    async fn zscore(&self, sorted_set_key: &str, member: &str) -> Result<i64, BoxError> {
        let score: String = self.storage.client().zscore(sorted_set_key, member).await?;
        Ok(score.parse()?)
    }

    async fn zcard(&self, sorted_set_key: &str) -> StorageResult<u64> {
        let cardinality = self.storage.client().zcard(sorted_set_key).await?;
        Ok(cardinality)
    }

    async fn zexists(&self, sorted_set_key: &str, member: &str) -> StorageResult<bool> {
        let score: Option<String> = self.storage.client().zscore(sorted_set_key, member).await?;
        Ok(score.is_some())
    }

    async fn exists(&self, key: &str) -> StorageResult<bool> {
        Ok(self.storage.client().exists(key).await?)
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
    use tokio::sync::broadcast;
    use tokio::time::Instant;
    use tower::BoxError;
    use uuid::Uuid;

    use super::Config;
    use super::Storage;
    use super::now;
    use crate::plugins::response_cache::ErrorCode;
    use crate::plugins::response_cache::storage::CacheStorage;
    use crate::plugins::response_cache::storage::Document;
    use crate::plugins::response_cache::storage::Error;

    const SUBGRAPH_NAME: &str = "test";

    fn redis_config(clustered: bool) -> Config {
        Config::test(clustered, &random_namespace())
    }

    fn random_namespace() -> String {
        Uuid::new_v4().to_string()
    }

    fn common_document() -> Document {
        Document {
            key: "key".to_string(),
            data: Default::default(),
            control: Default::default(),
            invalidation_keys: vec!["invalidate".to_string()],
            expire: Duration::from_secs(60),
            debug: true,
        }
    }

    #[tokio::test]
    #[rstest::rstest]
    async fn test_invalidation_key_permutations(
        #[values(None, Some("test"))] namespace: Option<&str>,
        #[values(vec![], vec!["invalidation"], vec!["invalidation1", "invalidation2", "invalidation3"])]
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
            ..redis_config(false)
        };
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::mocked(&config, false, mock_storage, drop_rx)
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
        use tokio::sync::broadcast;
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
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            // every element of this namespace must have a TTL associated with it, and the TTL of the
            // cache keys must be greater than the TTL of the document
            let document = common_document();
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            let document_key = storage.make_key(document.key.clone());
            let expected_cache_tag_keys =
                storage.namespaced_cache_tags(&document.invalidation_keys, SUBGRAPH_NAME);

            // iterate over all the keys in the namespace and make sure we have everything we'd expect
            let keys = storage.all_keys_in_namespace().await?;
            assert!(keys.contains(&document_key));
            for key in &expected_cache_tag_keys {
                assert!(keys.contains(key), "missing {key}");
            }
            assert_eq!(keys.len(), 3); // 1 document + 2 cache tags

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
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            // set up two documents with a shared key and different TTLs
            let documents = vec![
                Document {
                    key: "key1".to_string(),
                    invalidation_keys: vec![
                        "invalidation".to_string(),
                        "invalidation1".to_string(),
                    ],
                    expire: Duration::from_secs(30),
                    ..common_document()
                },
                Document {
                    key: "key2".to_string(),
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
                expected_document_keys.push(storage.make_key(&document.key));
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
            assert_eq!(keys.len(), 6); // 2 documents + 4 cache tags

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
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            let document = Document {
                key: "key1".to_string(),
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
                key: "key2".to_string(),
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
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            let document = Document {
                expire: Duration::from_secs(60),
                data: serde_json_bytes::Value::Number(1.into()),
                ..common_document()
            };
            let document_key = storage.make_key(document.key.clone());
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was stored
            let stored_data = storage.fetch(&common_document().key, SUBGRAPH_NAME).await?;
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
            let stored_data = storage.fetch(&document.key, SUBGRAPH_NAME).await?;
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
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            let document = Document {
                expire: Duration::from_secs(60),
                data: serde_json_bytes::Value::Number(1.into()),
                ..common_document()
            };
            let document_key = storage.make_key(document.key.clone());
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was stored
            let stored_data = storage.fetch(&common_document().key, SUBGRAPH_NAME).await?;
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
            let stored_data = storage.fetch(&document.key, SUBGRAPH_NAME).await?;
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

        use fred::error::Error;
        use fred::error::ErrorKind;
        use fred::interfaces::KeysInterface;
        use fred::mocks::MockCommand;
        use fred::mocks::Mocks;
        use fred::prelude::Expiration;
        use fred::prelude::Value;
        use parking_lot::RwLock;
        use tokio::sync::broadcast;
        use tower::BoxError;

        use super::SUBGRAPH_NAME;
        use super::common_document;
        use super::redis_config;
        use crate::plugins::response_cache::ErrorCode;
        use crate::plugins::response_cache::storage::CacheStorage;
        use crate::plugins::response_cache::storage::Document;
        use crate::plugins::response_cache::storage::redis::Storage;

        /// Trigger failure by pre-setting the cache tag to an invalid type.
        #[tokio::test]
        #[rstest::rstest]
        async fn type_failure(#[values(true, false)] clustered: bool) -> Result<(), BoxError> {
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let config = redis_config(clustered);
            let storage = Storage::new(&config, drop_rx).await?;
            storage.truncate_namespace().await?;

            let document = common_document();
            let document_key = storage.make_key(document.key.clone());
            let cache_tag_keys =
                storage.namespaced_cache_tags(&document.invalidation_keys, SUBGRAPH_NAME);

            let insert_invalid_cache_tag = |key: String| async {
                let _: () = storage
                    .storage
                    .client()
                    .set(key, 1, Some(Expiration::EX(60)), None, false)
                    .await?;
                Ok::<(), BoxError>(())
            };
            let inserted_data = |key: String| async {
                let exists = storage.storage.client().exists(key).await?;
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
                    key: "key1".to_string(),
                    invalidation_keys: vec![],
                    ..common_document()
                },
                Document {
                    key: "key2".to_string(),
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
                    assert!(!inserted_data(storage.make_key(document.key.clone())).await?);
                }
            }

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn timeout_failure(#[values(true, false)] clustered: bool) -> Result<(), BoxError> {
            use crate::plugins::response_cache::storage::error::Error as StorageError;

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

            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let mock_storage = Arc::new(MockStorage::default());
            let storage = Storage::mocked(
                &redis_config(clustered),
                clustered,
                mock_storage.clone(),
                drop_rx,
            )
            .await?;

            let document = common_document();
            let document_key = Value::from(storage.make_key(document.key.clone()));

            let result = storage.insert(document, SUBGRAPH_NAME).await;
            let error = result.expect_err("should have timed out via redis");
            assert!(matches!(error, StorageError::Database(ref e) if e.details() == "timeout"));
            assert_eq!(error.code(), "TIMEOUT");

            // make sure the insert function did not try to operate on the document key
            for command in mock_storage.0.read().iter() {
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
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
        storage.truncate_namespace().await?;

        // set up two documents with a shared key and different TTLs
        let documents = vec![
            Document {
                key: "key1".to_string(),
                expire: Duration::from_secs(2),
                ..common_document()
            },
            Document {
                key: "key2".to_string(),
                expire: Duration::from_secs(60),
                ..common_document()
            },
            Document {
                key: "key3".to_string(),
                expire: Duration::from_secs(60),
                ..common_document()
            },
        ];
        storage
            .insert_in_batch(documents.clone(), SUBGRAPH_NAME)
            .await?;

        // ensure that we have three elements in the 'whole-subgraph' invalidation key
        let invalidation_key = storage.namespaced_cache_tags(&[], SUBGRAPH_NAME).remove(0);
        assert_eq!(storage.zcard(&invalidation_key).await?, 3);

        let doc_key1 = storage.make_key("key1");
        let doc_key2 = storage.make_key("key2");
        let doc_key3 = storage.make_key("key3");
        for key in [&doc_key1, &doc_key2, &doc_key3] {
            assert!(storage.zexists(&invalidation_key, key).await?);
        }

        // manually trigger maintenance with a time in the future, in between the expiry times of doc1
        // and docs 2 and 3. therefore, we should remove `key1` and leave `key2` and `key3`
        let cutoff = now() + 10;
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
        let cutoff = now() + 1000;
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

    mod invalidation {
        use tokio::sync::broadcast;
        use tower::BoxError;

        use super::common_document;
        use super::redis_config;
        use crate::plugins::response_cache::storage::CacheStorage;
        use crate::plugins::response_cache::storage::Document;
        use crate::plugins::response_cache::storage::redis::Storage;

        #[tokio::test]
        #[rstest::rstest]
        async fn invalidation_by_subgraph_removes_everything_associated_with_that_subgraph(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            let document1 = Document {
                key: "key1".to_string(),
                ..common_document()
            };

            let document2 = Document {
                key: "key2".to_string(),
                ..common_document()
            };

            let document3 = Document {
                key: "key3".to_string(),
                ..common_document()
            };

            storage.insert(document1.clone(), "S1").await?;
            storage.insert(document2.clone(), "S2").await?;
            storage.insert(document3.clone(), "S2").await?;

            // invalidate just subgraph1
            let num_invalidated = storage.invalidate_by_subgraph("S1", "subgraph").await?;
            assert_eq!(num_invalidated, 1);
            assert!(!storage.exists(&storage.make_key("key1")).await?);
            assert!(storage.exists(&storage.make_key("key2")).await?);

            // invalidate subgraph2
            let num_invalidated = storage.invalidate_by_subgraph("S2", "subgraph").await?;
            assert_eq!(num_invalidated, 2);
            assert!(!storage.exists(&storage.make_key("key2")).await?);
            assert!(!storage.exists(&storage.make_key("key3")).await?);

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn arguments_are_restrictive_rather_than_additive(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            // invalidate takes a list of invalidation keys and a list of subgraphs; the two are combined
            // to form a list of cache tags to remove from
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            let document1 = Document {
                key: "key1".to_string(),
                invalidation_keys: vec!["A".to_string()],
                ..common_document()
            };

            let document2 = Document {
                key: "key2".to_string(),
                invalidation_keys: vec!["A".to_string()],
                ..common_document()
            };

            let document3 = Document {
                key: "key3".to_string(),
                invalidation_keys: vec!["B".to_string()],
                ..common_document()
            };

            storage.insert(document1.clone(), "S1").await?;
            storage.insert(document2.clone(), "S2").await?;
            storage.insert(document3.clone(), "S2").await?;

            // invalidate(A, S2) will invalidate key2, NOT key1 or key3
            let invalidated = storage
                .invalidate(vec!["A".to_string()], vec!["S2".to_string()], "cache_tag")
                .await?;
            assert_eq!(invalidated.len(), 1);
            assert_eq!(*invalidated.get("S2").unwrap(), 1);
            assert!(storage.exists(&storage.make_key("key1")).await?);
            assert!(!storage.exists(&storage.make_key("key2")).await?);
            assert!(storage.exists(&storage.make_key("key3")).await?);

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn invalidating_missing_subgraph_will_not_error(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            storage.insert(common_document(), "S1").await?;

            let invalidated = storage.invalidate_by_subgraph("S2", "subgraph").await?;
            assert_eq!(invalidated, 0);

            let invalidated = storage
                .invalidate(vec!["key".to_string()], vec!["S2".to_string()], "cache_tag")
                .await?;
            assert_eq!(invalidated.len(), 1);
            assert_eq!(*invalidated.get("S2").unwrap(), 0);

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn invalidating_missing_invalidation_key_will_not_error(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            storage.insert(common_document(), "S1").await?;

            let invalidated = storage
                .invalidate(vec!["key".to_string()], vec!["S1".to_string()], "cache_tag")
                .await?;
            assert_eq!(invalidated.len(), 1);
            assert_eq!(*invalidated.get("S1").unwrap(), 0);

            Ok(())
        }

        #[tokio::test]
        #[rstest::rstest]
        async fn invalidation_is_idempotent(
            #[values(true, false)] clustered: bool,
        ) -> Result<(), BoxError> {
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::new(&redis_config(clustered), drop_rx).await?;
            storage.truncate_namespace().await?;

            let document = common_document();
            let document_key = storage.make_key(&document.key);

            storage.insert(document, "S1").await?;
            assert!(storage.exists(&document_key).await?);

            let invalidated = storage.invalidate_by_subgraph("S1", "subgraph").await?;
            assert_eq!(invalidated, 1);

            assert!(!storage.exists(&document_key).await?);

            // re-invalidate - storage still shouldn't have the key in it, and it shouldn't
            // encounter an error
            let invalidated = storage.invalidate_by_subgraph("S1", "subgraph").await?;
            assert_eq!(invalidated, 0);
            assert!(!storage.exists(&document_key).await?);

            Ok(())
        }
    }

    #[tokio::test]
    async fn timeout_errors_are_captured() -> Result<(), BoxError> {
        let config = Config {
            fetch_timeout: Duration::from_nanos(0),
            ..redis_config(false)
        };
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(&config, drop_rx).await?;
        storage.truncate_namespace().await?;

        let document = common_document();

        // because of how tokio::timeout polls, it's possible for a command to finish before the
        // timeout is polled (even if the duration is 0). perform the check in a loop to give it
        // a few changes to trigger.
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(5) {
            let error = storage.fetch(&document.key, "S1").await.unwrap_err();
            if error.is_row_not_found() {
                continue;
            }

            assert!(matches!(error, Error::Timeout(_)), "{:?}", error);
            assert_eq!(error.code(), "TIMEOUT");
            return Ok(());
        }

        panic!("Never observed a timeout");
    }
}
