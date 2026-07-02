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
use crate::metrics::FutureMetricsExt;
use crate::plugins::response_cache::cache_control::CacheControl;
use crate::plugins::response_cache::metrics::record_maintenance_commands;
use crate::plugins::response_cache::metrics::record_maintenance_duration;
use crate::plugins::response_cache::metrics::record_maintenance_error;
use crate::plugins::response_cache::metrics::record_maintenance_queue_error;
use crate::plugins::response_cache::metrics::record_maintenance_success;
use crate::plugins::response_cache::plugin::RESPONSE_CACHE_VERSION;

pub(crate) type Config = super::config::Config;

const CACHE_TAG_CHANNEL_SIZE: usize = 1000;

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

        // NOTE: this is a bit of a dance, but we have to create a RedisCacheStorage before we can
        // create its wrapped client because we want that client to be replaceable and need a
        // standalone data container for certain fields used for its creation (and thus
        // recreation)
        let storage = RedisCacheStorage::new(config.into(), "response-cache").await?;
        // WARN: don't skip creating the client; the RedisCacheStorage::new() starts with a None as
        // for wrapped client
        storage.clone().create_client_pool().await?;

        let (cache_tag_tx, cache_tag_rx) = mpsc::channel(CACHE_TAG_CHANNEL_SIZE);
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

    /// Activate the Redis storage so it can start emitting metrics.
    pub(crate) fn activate(&self) {
        self.storage.activate();
    }

    fn make_key<K: KeyType>(&self, key: K) -> String {
        self.storage.make_key(RedisKey(key))
    }

    async fn invalidate_keys(&self, invalidation_keys: Vec<String>) -> StorageResult<u64> {
        let options = Options {
            timeout: Some(self.invalidate_timeout()),
            ..Options::default()
        };
        let pipeline = self.storage.pipeline().await?.with_options(&options);
        for invalidation_key in &invalidation_keys {
            let invalidation_key =
                format!("version:{RESPONSE_CACHE_VERSION}:cache-tag:{invalidation_key}");
            self.send_to_maintenance_queue(invalidation_key.clone());

            let redis_key = self.make_key(invalidation_key.clone());
            let _: () = pipeline
                .zrange(redis_key, 0, -1, None, false, None, false)
                .await?;
        }

        let results: Vec<Vec<String>> = pipeline.all().await?;
        let all_keys: HashSet<String> = results.into_iter().flatten().collect();
        if all_keys.is_empty() {
            return Ok(0);
        }

        // add namespace to keys
        let keys = all_keys
            .into_iter()
            .map(|key| self.make_key(key))
            .map(fred::types::Key::from);
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
        tokio::spawn(
            async move {
                loop {
                    tokio::select! {
                            biased;
                            _ = drop_rx.recv() => break,
                            Some(first_cache_tag) = cache_tag_rx.recv() => {
                            // The main method of deduplication: using a HashSet. This will keep our
                            // total commands sent to redis for ZSET removal lower than otherwise would
                            // be the case
                            let mut keys = HashSet::new();
                            let mut deduplicated_commands = 0u64;
                            keys.insert(first_cache_tag);
                            // We make sure that we have a hard limit for how long we loop, just in
                            // case more tags are added to the queue than we can process at a time
                            for _ in 0..CACHE_TAG_CHANNEL_SIZE {
                                match cache_tag_rx.try_recv() {
                                    Ok(key) => {
                                        if !keys.insert(key) {
                                            deduplicated_commands += 1;
                                        }
                                    }
                                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                                    Err(err) => {
                                        tracing::debug!("maintenance queue disconnected: {err}");
                                        break
                                    }
                                }
                            }

                            record_maintenance_commands(
                                deduplicated_commands,
                                keys.len() as u64,
                            );

                            for key in keys {
                                storage.perform_maintenance_on_cache_tag(key).await
                            }
                        }
                    }
                }
            }
            .with_current_meter_provider(),
        );
    }

    async fn perform_maintenance_on_cache_tag(&self, cache_tag: String) {
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

        // NB: add namespace to cache tag
        let cache_tag_key = self.make_key(cache_tag_key);
        Ok(self
            .storage
            .client()
            .await?
            .with_options(&options)
            .zremrangebyscore(&cache_tag_key, f64::NEG_INFINITY, cutoff_time)
            .await?)
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
        batch_docs: Vec<Document>,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        // three phases:
        //   1 - render each document's cache-tag entries into Redis ZSET keys
        //   2 - ZADD each cache-tag ZSET with (expiry, document_key) memberships
        //   3 - insert each document's data and snapshot tags for the debugger
        // a failure in any phase will cause the function to return, which prevents invalid states

        let now = now();

        // Snapshot each document's user-facing tag values (CacheTag::Tag). These are persisted on
        // every CacheEntry so cache hits replay the same tag set as misses for supergraph response
        // cache-tag propagation (router#9481), and are also surfaced to the cache debugger.
        // Internal tags (Subgraph and Type) are not surfaced to operators.
        let user_cache_tags: Vec<Vec<String>> = batch_docs
            .iter()
            .map(|document| {
                document
                    .cache_tags
                    .iter()
                    .filter_map(|t| t.user_value().map(String::from))
                    .collect()
            })
            .collect();

        // phase 1: render every document's tag list to its Redis ZSET keys.
        let redis_keys_per_doc: Vec<Vec<String>> = batch_docs
            .iter()
            .map(|document| {
                document
                    .cache_tags
                    .iter()
                    .map(|tag| tag.to_redis_key(subgraph_name))
                    .collect()
            })
            .collect();

        // phase 2: build the per-ZSET membership list and ZADD each one.
        let num_cache_tags_estimate = 2 * batch_docs.len();
        let mut cache_tags_to_pcks: HashMap<String, Vec<(f64, String)>> =
            HashMap::with_capacity(num_cache_tags_estimate);
        for (document, redis_keys) in batch_docs.iter().zip(redis_keys_per_doc.iter()) {
            let cache_tag_value = (
                (now + document.expire.as_secs()) as f64,
                document.key.clone(),
            );
            for redis_key in redis_keys {
                if let Some(entry) = cache_tags_to_pcks.get_mut(redis_key) {
                    entry.push(cache_tag_value.clone());
                } else {
                    cache_tags_to_pcks.insert(redis_key.clone(), vec![cache_tag_value.clone()]);
                }
            }
        }

        let options = Options {
            timeout: Some(self.insert_timeout()),
            ..Options::default()
        };
        let pipeline = self.storage.pipeline().await?.with_options(&options);
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

            let redis_key = self.make_key(cache_tag_key);

            let _: Result<(), _> = pipeline
                .zadd(
                    redis_key.clone(),
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
                    .expire_at(redis_key.clone(), cache_tag_expiry_time, Some(exp_opt))
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
        let pipeline = self.storage.pipeline().await?.with_options(&options);
        for (document, tags) in batch_docs.into_iter().zip(user_cache_tags) {
            let value = CacheValue {
                data: document.data,
                cache_control: document.control,
                // Always persist the tag set (router#9481) so cache hits replay it. `None` is
                // reserved for legacy entries written before the feature shipped.
                cache_tags: Some(tags.into_iter().collect()),
            };
            let _: () = pipeline
                .set::<(), _, _>(
                    self.make_key(document.key),
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
    ) -> StorageResult<Vec<StorageResult<CacheEntry>>> {
        let keys: Vec<RedisKey<String>> = cache_keys
            .iter()
            .map(|key| RedisKey(key.to_string()))
            .collect();
        let options = Options {
            timeout: Some(self.fetch_timeout()),
            ..Options::default()
        };
        let values: Vec<Result<RedisValue<CacheValue>, _>> = self
            .storage
            .get_multiple_with_options(keys, options)
            .await?;

        let entries = values
            .into_iter()
            .zip(cache_keys)
            .map(|(opt_value, cache_key)| {
                opt_value
                    .map(|value| CacheEntry::from((*cache_key, value.0)))
                    .map_err(Into::into)
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
            counts.insert(subgraph_name, count?);
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

    /// Return a list of all keys in this namespace, with the namespace string stripped from
    /// each key.
    async fn all_keys_in_namespace(&self) -> Result<Vec<String>, BoxError> {
        use fred::types::scan::Scanner;
        use tokio_stream::StreamExt;

        let mut scan_stream = self
            .storage
            .scan_with_namespaced_results(String::from("*"), None)
            .await
            .map_err(BoxError::from)?;

        let mut keys = Vec::default();
        while let Some(result) = scan_stream.next().await {
            if let Some(page_keys) = result?.take_results() {
                let mut str_keys: Vec<String> = page_keys
                    .into_iter()
                    .map(|k| k.into_string().unwrap())
                    .map(|k| self.storage.strip_namespace(k))
                    .collect();
                keys.append(&mut str_keys);
            }
        }

        Ok(keys)
    }

    async fn ttl(&self, key: &str) -> StorageResult<i64> {
        let key = self.make_key(key);
        Ok(self.storage.client().await?.ttl(key).await?)
    }

    async fn expire_time(&self, key: &str) -> StorageResult<i64> {
        let key = self.make_key(key);
        Ok(self.storage.client().await?.expire_time(key).await?)
    }

    async fn zscore(&self, sorted_set_key: &str, member: &str) -> Result<i64, BoxError> {
        let sorted_set_key = self.make_key(sorted_set_key);
        let score: String = self
            .storage
            .client()
            .await?
            .zscore(sorted_set_key, member)
            .await?;
        Ok(score.parse()?)
    }

    async fn zcard(&self, sorted_set_key: &str) -> StorageResult<u64> {
        let sorted_set_key = self.make_key(sorted_set_key);
        let cardinality = self.storage.client().await?.zcard(sorted_set_key).await?;
        Ok(cardinality)
    }

    async fn zexists(&self, sorted_set_key: &str, member: &str) -> StorageResult<bool> {
        let sorted_set_key = self.make_key(sorted_set_key);
        let score: Option<String> = self
            .storage
            .client()
            .await?
            .zscore(sorted_set_key, member)
            .await?;
        Ok(score.is_some())
    }

    async fn exists(&self, key: &str) -> StorageResult<bool> {
        let key = self.make_key(key);
        Ok(self.storage.client().await?.exists(key).await?)
    }

    pub(crate) fn send_to_maintenance_queue_for_test(&self, key: String) {
        self.send_to_maintenance_queue(key);
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
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::response_cache::ErrorCode;
    use crate::plugins::response_cache::cache_tag::CacheTag;
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

    /// Test helper: render the Redis ZSET keys a document indexes under. Mirrors the storage
    /// layer's per-document rendering for assertion convenience in tests.
    fn render_doc_keys(document: &Document, subgraph_name: &str) -> Vec<String> {
        document
            .cache_tags
            .iter()
            .map(|t| t.to_redis_key(subgraph_name))
            .collect()
    }

    /// Test helper: render the Redis ZSET keys an explicit cache-tag list indexes under.
    fn render_tag_keys(tags: &[CacheTag], subgraph_name: &str) -> Vec<String> {
        tags.iter().map(|t| t.to_redis_key(subgraph_name)).collect()
    }

    fn common_document() -> Document {
        Document {
            key: "key".to_string(),
            data: Default::default(),
            control: Default::default(),
            cache_tags: vec![CacheTag::Subgraph, CacheTag::Tag("invalidate".to_string())],
            expire: Duration::from_secs(60),
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
        let _storage = Storage::mocked(&config, false, mock_storage, drop_rx)
            .await
            .expect("could not build storage");

        let invalidation_keys: Vec<String> = invalidation_keys
            .into_iter()
            .map(ToString::to_string)
            .collect();

        let mut cache_tags = render_tag_keys(
            &{
                let mut tags = vec![CacheTag::Subgraph];
                tags.extend(invalidation_keys.iter().cloned().map(CacheTag::Tag));
                tags
            },
            "products",
        );
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
        use super::*;
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

            let document_key = document.key.clone();
            let expected_cache_tag_keys = render_doc_keys(&document, SUBGRAPH_NAME);

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
                    cache_tags: vec![
                        CacheTag::Subgraph,
                        CacheTag::Tag("invalidation".to_string()),
                        CacheTag::Tag("invalidation1".to_string()),
                    ],
                    expire: Duration::from_secs(30),
                    ..common_document()
                },
                Document {
                    key: "key2".to_string(),
                    cache_tags: vec![
                        CacheTag::Subgraph,
                        CacheTag::Tag("invalidation".to_string()),
                        CacheTag::Tag("invalidation2".to_string()),
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
                expected_document_keys.push(document.key.clone());
                expected_cache_tag_keys.push(render_doc_keys(document, SUBGRAPH_NAME));
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
            let document_key = document.key.clone();
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was stored
            let stored_data = storage.fetch(&document_key, SUBGRAPH_NAME).await?;
            assert_eq!(stored_data.data, document.data);

            let keys = render_doc_keys(&document, SUBGRAPH_NAME);

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
            let document_key = document.key.clone();
            storage.insert(document.clone(), SUBGRAPH_NAME).await?;

            // make sure the document was stored
            let stored_data = storage.fetch(&common_document().key, SUBGRAPH_NAME).await?;
            assert_eq!(stored_data.data, document.data);

            let keys = render_doc_keys(&document, SUBGRAPH_NAME);

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
        use super::*;
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
            let document_key = document.key.clone();
            let cache_tag_keys = render_doc_keys(&document, SUBGRAPH_NAME);

            let insert_invalid_cache_tag = |key: String| async {
                let namespaced_key = storage.make_key(key);
                let _: () = storage
                    .storage
                    .client()
                    .await?
                    .set(namespaced_key, 1, Some(Expiration::EX(60)), None, false)
                    .await?;
                Ok::<(), BoxError>(())
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

                assert!(!storage.exists(&document_key).await?);
            }

            // this should also be true if inserting multiple documents, even if only one of the
            // documents' cache tags couldn't be updated.
            let documents = vec![
                Document {
                    key: "key1".to_string(),
                    cache_tags: vec![CacheTag::Subgraph],
                    ..common_document()
                },
                Document {
                    key: "key2".to_string(),
                    cache_tags: vec![CacheTag::Subgraph, CacheTag::Tag("invalidate".to_string())],
                    ..common_document()
                },
            ];

            let cache_tag_keys = render_doc_keys(&documents[1], SUBGRAPH_NAME);
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
                    assert!(!storage.exists(&document.key).await?);
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
        let invalidation_key = render_tag_keys(&[CacheTag::Subgraph], SUBGRAPH_NAME).remove(0);
        assert_eq!(storage.zcard(&invalidation_key).await?, 3);

        let doc_key1 = "key1";
        let doc_key2 = "key2";
        let doc_key3 = "key3";
        for key in [&doc_key1, &doc_key2, &doc_key3] {
            assert!(storage.zexists(&invalidation_key, key).await?);
        }

        // manually trigger maintenance with a time in the future, in between the expiry times of doc1
        // and docs 2 and 3. therefore, we should remove `key1` and leave `key2` and `key3`
        let cutoff = now() + 10;
        assert!(storage.zscore(&invalidation_key, doc_key1).await? < cutoff as i64);
        let removed_keys = storage
            .remove_keys_from_cache_tag_by_cutoff(invalidation_key.clone(), cutoff as f64)
            .await?;
        assert_eq!(removed_keys, 1);

        // now we should have two elements in the 'whole-subgraph' invalidation key
        assert_eq!(storage.zcard(&invalidation_key).await?, 2);
        assert!(!storage.zexists(&invalidation_key, doc_key1).await?);
        assert!(storage.zexists(&invalidation_key, doc_key2).await?);
        assert!(storage.zexists(&invalidation_key, doc_key3).await?);

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
        use super::*;
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
            assert!(!storage.exists("key1").await?);
            assert!(storage.exists("key2").await?);

            // invalidate subgraph2
            let num_invalidated = storage.invalidate_by_subgraph("S2", "subgraph").await?;
            assert_eq!(num_invalidated, 2);
            assert!(!storage.exists("key2").await?);
            assert!(!storage.exists("key3").await?);

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
                cache_tags: vec![CacheTag::Subgraph, CacheTag::Tag("A".to_string())],
                ..common_document()
            };

            let document2 = Document {
                key: "key2".to_string(),
                cache_tags: vec![CacheTag::Subgraph, CacheTag::Tag("A".to_string())],
                ..common_document()
            };

            let document3 = Document {
                key: "key3".to_string(),
                cache_tags: vec![CacheTag::Subgraph, CacheTag::Tag("B".to_string())],
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
            assert!(storage.exists("key1").await?);
            assert!(!storage.exists("key2").await?);
            assert!(storage.exists("key3").await?);

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
            let document_key = document.key.clone();

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

    /// Tests specific to the maintenance consumer's batch-drain and deduplication behaviour.
    mod maintenance_consumer {
        use std::collections::HashMap;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        use fred::error::Error;
        use fred::error::ErrorKind;
        use fred::mocks::MockCommand;
        use fred::mocks::Mocks;
        use fred::types::Value;
        use parking_lot::Mutex;
        use tokio::sync::broadcast;
        use tower::BoxError;

        use super::SUBGRAPH_NAME;
        use super::Storage;
        use super::common_document;
        use super::redis_config;
        use crate::metrics::FutureMetricsExt;
        use crate::plugins::response_cache::storage::CacheStorage;

        /// Records every ZREMRANGEBYSCORE call made by the maintenance worker.
        #[derive(Default, Clone, Debug)]
        struct RecordingMocks {
            /// key → call count
            calls: Arc<Mutex<HashMap<String, usize>>>,
        }

        impl RecordingMocks {
            fn total_calls(&self) -> usize {
                self.calls.lock().values().sum()
            }

            fn unique_keys_called(&self) -> usize {
                self.calls.lock().len()
            }
        }

        impl Mocks for RecordingMocks {
            fn process_command(&self, command: MockCommand) -> Result<Value, Error> {
                if command.cmd == "ZREMRANGEBYSCORE" {
                    let key = command
                        .args
                        .first()
                        .and_then(|v| v.clone().into_string())
                        .unwrap_or_default();
                    *self.calls.lock().entry(key).or_insert(0) += 1;
                    return Ok(Value::Integer(0));
                }
                Ok(Value::Integer(0))
            }
        }

        /// Returns an error for any ZREMRANGEBYSCORE call whose key contains `error_key_fragment`;
        /// counts all successful ZREMRANGEBYSCORE calls in `successful_calls`.
        #[derive(Debug, Clone)]
        struct SelectiveErrorMocks {
            error_key_fragment: String,
            successful_calls: Arc<AtomicUsize>,
        }

        impl Mocks for SelectiveErrorMocks {
            fn process_command(&self, command: MockCommand) -> Result<Value, Error> {
                if command.cmd == "ZREMRANGEBYSCORE" {
                    let key = command
                        .args
                        .first()
                        .and_then(|v| v.clone().into_string())
                        .unwrap_or_default();
                    if key.contains(&self.error_key_fragment) {
                        return Err(Error::new(ErrorKind::Unknown, "simulated redis error"));
                    }
                    self.successful_calls.fetch_add(1, Ordering::SeqCst);
                    return Ok(Value::Integer(0));
                }
                Ok(Value::Integer(0))
            }
        }

        /// Yields to the tokio scheduler until `condition` is true, with a 5-second hard timeout.
        async fn wait_for(condition: impl Fn() -> bool) {
            let timeout = tokio::time::Instant::now() + Duration::from_secs(5);
            while !condition() {
                assert!(
                    tokio::time::Instant::now() < timeout,
                    "timed out waiting for condition"
                );
                tokio::task::yield_now().await;
            }
        }

        /// When the same cache-tag key is queued N times before the consumer runs, the drain loop
        /// collapses all N copies into one HashSet entry and issues a single ZREMRANGEBYSCORE call.
        #[tokio::test]
        async fn deduplicates_same_key_in_batch() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            // All 50 sends are synchronous (try_send, no await). The consumer task cannot run
            // until we yield, so all 50 items land in the channel before the first drain.
            let key = "test-dedup-key".to_string();
            for _ in 0..50 {
                storage.send_to_maintenance_queue_for_test(key.clone());
            }

            // Yield until at least one ZREMRANGEBYSCORE has been recorded.
            wait_for(|| mock.total_calls() >= 1).await;

            // 50 identical sends → one unique key → at most a handful of Redis calls (one per
            // drain batch), nowhere near 50.
            assert!(
                mock.total_calls() < 50,
                "expected deduplication to reduce 50 identical sends to far fewer Redis calls, \
                 got {}",
                mock.total_calls()
            );

            Ok(())
        }

        /// All distinct keys queued before the consumer runs must each receive their own
        /// ZREMRANGEBYSCORE call — none should be silently skipped.
        #[tokio::test]
        async fn processes_all_distinct_keys() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            let n = 10usize;
            for i in 0..n {
                storage.send_to_maintenance_queue_for_test(format!("test-distinct-key-{i}"));
            }

            // Wait until all n distinct keys have each been seen at least once.
            wait_for(|| mock.unique_keys_called() >= n).await;

            assert_eq!(
                mock.unique_keys_called(),
                n,
                "all {n} distinct keys should have been maintained"
            );

            Ok(())
        }

        /// Sending more items than the channel capacity must not panic. Items that fit are still
        /// processed; overflowing items are silently dropped (recorded as queue errors).
        #[tokio::test]
        async fn channel_overflow_does_not_panic() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            // The mock channel capacity is 100. Sending 200 items will overflow it; the second
            // 100 are dropped. The first 100 distinct keys should still be processed.
            for i in 0..200 {
                storage.send_to_maintenance_queue_for_test(format!("test-overflow-key-{i}"));
            }

            wait_for(|| mock.total_calls() >= 1).await;

            assert!(
                mock.total_calls() <= 200,
                "expected at most 200 Redis calls, got {}",
                mock.total_calls()
            );

            Ok(())
        }

        /// After the consumer drains and processes one batch it must block on recv() and then pick
        /// up subsequent items when they arrive — the loop must not exit after the first batch.
        #[tokio::test]
        async fn processes_subsequent_batches() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            // First batch: 5 distinct keys.
            for i in 0..5 {
                storage.send_to_maintenance_queue_for_test(format!("test-batch1-{i}"));
            }
            wait_for(|| mock.unique_keys_called() >= 5).await;

            // Second batch: 5 more distinct keys sent after the first batch is consumed.
            for i in 0..5 {
                storage.send_to_maintenance_queue_for_test(format!("test-batch2-{i}"));
            }
            wait_for(|| mock.unique_keys_called() >= 10).await;

            assert_eq!(
                mock.unique_keys_called(),
                10,
                "consumer should have processed both batches (10 unique keys total)"
            );

            Ok(())
        }

        /// A realistic traffic mix: many copies of a hot key alongside a handful of cooler keys.
        /// The HashSet must collapse the hot-key duplicates while still giving every cooler key
        /// its own ZREMRANGEBYSCORE call — none should be starved or skipped.
        #[tokio::test]
        async fn deduplicates_hot_key_while_preserving_cool_keys() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            // Simulate one hot subgraph ZSET flooding the queue alongside 4 cooler ones.
            let hot = "hot-subgraph-key";
            let cool_count = 4usize;
            for _ in 0..20 {
                storage.send_to_maintenance_queue_for_test(hot.to_string());
            }
            for i in 0..cool_count {
                storage.send_to_maintenance_queue_for_test(format!("cool-key-{i}"));
            }

            // Wait until all distinct keys have been seen.
            wait_for(|| mock.unique_keys_called() > cool_count).await;

            // Exactly 5 unique keys, each processed at least once.
            assert_eq!(mock.unique_keys_called(), cool_count + 1);
            // Deduplification must have collapsed the 20 hot-key sends to far fewer calls.
            assert!(
                mock.total_calls() < 20,
                "expected deduplication to collapse 20 hot-key sends, got {} total calls",
                mock.total_calls()
            );

            Ok(())
        }

        /// After the consumer processes the first item via `recv()`, the drain loop calls
        /// `try_recv()`. When the channel is empty it must observe `Empty` and break cleanly
        /// rather than spin. Note: dropping `storage` does NOT disconnect the channel — the
        /// spawned worker task holds its own `Storage` clone (and thus its own sender), so
        /// `Disconnected` is structurally unreachable. This test verifies the `Empty` break path.
        #[tokio::test]
        async fn idle_channel_terminates_drain_loop_cleanly() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            // Queue exactly one item. After the consumer pulls it via recv() and enters the drain
            // loop, try_recv() immediately observes Empty and breaks — no extra Redis calls.
            storage.send_to_maintenance_queue_for_test("idle-test-key".to_string());

            wait_for(|| mock.total_calls() >= 1).await;

            // Exactly one ZREMRANGEBYSCORE call for the single queued key — no extra calls from
            // the drain loop spinning on an empty channel.
            assert_eq!(mock.total_calls(), 1);

            Ok(())
        }

        /// When the shutdown signal fires before the consumer task has had a chance to run,
        /// the biased select must observe the drop signal first and exit without processing any
        /// queued items — and must not panic.
        #[tokio::test]
        async fn shutdown_with_items_queued_does_not_panic() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            // Queue 10 items without yielding so the consumer has not yet run.
            for i in 0..10 {
                storage.send_to_maintenance_queue_for_test(format!("shutdown-key-{i}"));
            }

            // Fire the shutdown signal. Because no awaits have happened since the sends, the
            // consumer task is still parked. On the next select iteration it checks drop_rx first
            // (biased) and breaks immediately without processing anything.
            drop(drop_tx);

            tokio::task::yield_now().await;
            tokio::task::yield_now().await;

            // No panic is the primary assertion. The items should not have been processed since
            // the shutdown signal took priority.
            assert_eq!(
                mock.total_calls(),
                0,
                "consumer should have exited via shutdown before processing any items"
            );

            Ok(())
        }

        /// A key that appears in both batch 1 and batch 2 must be processed in both. The
        /// HashSet is local to each drain iteration; if it were accidentally shared across
        /// outer-loop iterations, the key would be silently skipped the second time.
        #[tokio::test]
        async fn same_key_is_processed_in_each_new_batch() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            let key = "repeated-across-batches".to_string();

            // Batch 1.
            storage.send_to_maintenance_queue_for_test(key.clone());
            wait_for(|| mock.total_calls() >= 1).await;
            let after_batch1 = mock.total_calls();

            // Batch 2 — same key. Consumer must process it again, not skip it.
            storage.send_to_maintenance_queue_for_test(key.clone());
            wait_for(|| mock.total_calls() > after_batch1).await;

            assert!(
                mock.total_calls() >= 2,
                "key should have been processed once per batch, \
                 got {} total calls",
                mock.total_calls()
            );

            Ok(())
        }

        /// A ZREMRANGEBYSCORE error on one ZSET key must not prevent the consumer from
        /// processing the remaining keys in the same batch. Each key's error is isolated.
        #[tokio::test]
        async fn redis_error_on_one_key_does_not_skip_others() -> Result<(), BoxError> {
            let successful = Arc::new(AtomicUsize::new(0));
            let mock = Arc::new(SelectiveErrorMocks {
                error_key_fragment: "error-key".to_string(),
                successful_calls: successful.clone(),
            });
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage = Storage::mocked(&redis_config(false), false, mock, drop_rx).await?;

            // One key will fail, two will succeed. All three are in the same drain batch.
            storage.send_to_maintenance_queue_for_test("error-key".to_string());
            storage.send_to_maintenance_queue_for_test("good-key-1".to_string());
            storage.send_to_maintenance_queue_for_test("good-key-2".to_string());

            wait_for(|| successful.load(Ordering::SeqCst) >= 2).await;

            assert_eq!(
                successful.load(Ordering::SeqCst),
                2,
                "both good keys should have been processed despite the error on error-key"
            );

            Ok(())
        }

        /// When `try_send` fails because the channel is full, `record_maintenance_queue_error`
        /// must be incremented. This is the primary signal operators use to detect a backlogged
        /// maintenance consumer.
        #[tokio::test]
        async fn channel_overflow_records_queue_error_metric() -> Result<(), BoxError> {
            async move {
                let mock = Arc::new(RecordingMocks::default());
                let (_drop_tx, drop_rx) = broadcast::channel(2);
                let storage =
                    Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

                // The mock channel capacity is 100. Sending 200 items means the second 100
                // each fail with TrySendError::Full, incrementing the counter exactly 100 times.
                // All 200 sends are synchronous (no awaits), so the consumer cannot drain
                // the channel between sends, keeping the count deterministic.
                for i in 0..200 {
                    storage.send_to_maintenance_queue_for_test(format!("overflow-metric-key-{i}"));
                }

                assert_counter!(
                    "apollo.router.operations.response_cache.maintenance.queue.error",
                    100,
                    "error" = "channel full"
                );

                Ok(())
            }
            .with_metrics()
            .await
        }

        /// Sending 10 identical keys synchronously guarantees they land in one batch:
        /// 10 raw → 1 unique executed, 9 deduplicated. Both sides of the ratio must be recorded.
        #[tokio::test]
        async fn deduplication_records_commands_metric() -> Result<(), BoxError> {
            async move {
                let mock = Arc::new(RecordingMocks::default());
                let (_drop_tx, drop_rx) = broadcast::channel(2);
                let storage =
                    Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

                let key = "dedup-metric-test-key".to_string();
                for _ in 0..10 {
                    storage.send_to_maintenance_queue_for_test(key.clone());
                }

                wait_for(|| mock.total_calls() >= 1).await;

                assert_counter!(
                    "experimental.apollo.router.operations.response_cache.maintenance.commands",
                    9,
                    "deduplicated" = "true"
                );
                assert_counter!(
                    "experimental.apollo.router.operations.response_cache.maintenance.commands",
                    1,
                    "deduplicated" = "false"
                );

                Ok(())
            }
            .with_metrics()
            .await
        }

        /// Verifies that `insert()` — the real production path — actually wires through to
        /// `send_to_maintenance_queue`, causing the background worker to call ZREMRANGEBYSCORE.
        /// This covers the gap left by tests that use `send_to_maintenance_queue_for_test`
        /// directly, which bypass `internal_insert_in_batch` entirely.
        #[tokio::test]
        async fn insert_wires_to_maintenance_queue() -> Result<(), BoxError> {
            let mock = Arc::new(RecordingMocks::default());
            let (_drop_tx, drop_rx) = broadcast::channel(2);
            let storage =
                Storage::mocked(&redis_config(false), false, mock.clone(), drop_rx).await?;

            // `insert` calls `internal_insert_in_batch`, which calls `send_to_maintenance_queue`
            // for each distinct ZSET key before executing the pipeline. Whether the pipeline
            // itself succeeds or fails is irrelevant — the queue send happens first.
            let _ = storage.insert(common_document(), SUBGRAPH_NAME).await;

            // The background worker must have picked up at least one ZSET key.
            wait_for(|| mock.total_calls() >= 1).await;

            assert!(
                mock.total_calls() >= 1,
                "insert() must wire through to the maintenance queue"
            );

            Ok(())
        }
    }

    #[tokio::test]
    async fn timeout_errors_are_captured() -> Result<(), BoxError> {
        async move {
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
                // NotFound manifests as Ok<Vec<None>> with fetch multiple, try again if that is the case
                let error = match storage.fetch_multiple(&[&document.key], "S1").await {
                    Ok(v) => {
                        if v.iter().all(|e| e.is_none()) {
                            continue;
                        }
                        panic!("Value was unexpected");
                    }
                    Err(err) => err,
                };

                assert!(matches!(error, Error::Timeout(_)), "{:?}", error);
                assert_eq!(error.code(), "TIMEOUT");
                assert_counter!(
                    "apollo.router.operations.response_cache.fetch.error",
                    1,
                    "code" = "TIMEOUT",
                    "subgraph.name" = "S1"
                );
                return Ok(());
            }

            panic!("Never observed a timeout");
        }
        .with_metrics()
        .await
    }
}
