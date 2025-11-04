mod config;
mod error;
pub(super) mod redis;

use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;

pub(super) use error::Error;
use tokio_util::future::FutureExt;

use super::cache_control::CacheControl;
use crate::plugins::response_cache::invalidation::InvalidationKind;
use crate::plugins::response_cache::metrics::record_fetch_duration;
use crate::plugins::response_cache::metrics::record_fetch_error;
use crate::plugins::response_cache::metrics::record_insert_duration;
use crate::plugins::response_cache::metrics::record_insert_error;
use crate::plugins::response_cache::metrics::record_invalidation_duration;

type StorageResult<T> = Result<T, Error>;

/// A `Document` is a unit of data to be stored in the cache, including any invalidation keys, its
/// TTL, cache control information, etc.
#[derive(Debug, Clone)]
pub(super) struct Document {
    pub(super) key: String,
    pub(super) data: serde_json_bytes::Value,
    pub(super) control: CacheControl,
    pub(super) invalidation_keys: Vec<String>,
    pub(super) expire: Duration,
    pub(super) debug: bool,
}

/// A `CacheEntry` is a unit of data returned from the cache. It contains the cache key, value, and
/// cache_control data.
#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    pub(super) key: String,
    pub(super) data: serde_json_bytes::Value,
    pub(super) control: CacheControl,
    // Only set in debug mode
    pub(super) cache_tags: Option<HashSet<String>>,
}

/// The `CacheStorage` trait defines an API that the backing storage layer must implement for
/// response caching: fetch, insert, and invalidate.
pub(super) trait CacheStorage {
    /// Timeout to apply to insert command.
    fn insert_timeout(&self) -> Duration;

    /// Timeout to apply to fetch command.
    fn fetch_timeout(&self) -> Duration;

    /// Timeout to apply to invalidate command.
    fn invalidate_timeout(&self) -> Duration;

    #[doc(hidden)]
    async fn internal_insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()>;

    /// Insert the `document` obtained from `subgraph_name`. Command will be timed out after
    /// `self.insert_timeout()`.
    async fn insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        let now = Instant::now();
        let result = flatten_storage_error(
            self.internal_insert(document, subgraph_name)
                .timeout(self.insert_timeout())
                .await,
        );

        record_insert_duration(now.elapsed(), subgraph_name, 1);
        result.inspect_err(|err| record_insert_error(err, subgraph_name))
    }

    #[doc(hidden)]
    async fn internal_insert_in_batch(
        &self,
        documents: Vec<Document>,
        subgraph_name: &str,
    ) -> StorageResult<()>;

    /// Insert the `document`s obtained from `subgraph_name`. Command will be timed out after
    /// `self.insert_timeout()`.
    async fn insert_in_batch(
        &self,
        documents: Vec<Document>,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        let batch_size = documents.len();

        let now = Instant::now();
        let result = flatten_storage_error(
            self.internal_insert_in_batch(documents, subgraph_name)
                .timeout(self.insert_timeout())
                .await,
        );

        record_insert_duration(now.elapsed(), subgraph_name, batch_size);
        result.inspect_err(|err| record_insert_error(err, subgraph_name))
    }

    #[doc(hidden)]
    async fn internal_fetch(&self, cache_key: &str) -> StorageResult<CacheEntry>;

    /// Fetch the value belonging to `cache_key`. Command will be timed out after `self.fetch_timeout()`.
    async fn fetch(&self, cache_key: &str, subgraph_name: &str) -> StorageResult<CacheEntry> {
        let now = Instant::now();
        let result = flatten_storage_error(
            self.internal_fetch(cache_key)
                .timeout(self.fetch_timeout())
                .await,
        );

        record_fetch_duration(now.elapsed(), subgraph_name, 1);
        result.inspect_err(|err| record_fetch_error(err, subgraph_name))
    }

    #[doc(hidden)]
    async fn internal_fetch_multiple(
        &self,
        cache_keys: &[&str],
    ) -> StorageResult<Vec<Option<CacheEntry>>>;

    /// Fetch the values belonging to `cache_keys`. Command will be timed out after `self.fetch_timeout()`.
    async fn fetch_multiple(
        &self,
        cache_keys: &[&str],
        subgraph_name: &str,
    ) -> StorageResult<Vec<Option<CacheEntry>>> {
        let batch_size = cache_keys.len();

        let now = Instant::now();
        let result = flatten_storage_error(
            self.internal_fetch_multiple(cache_keys)
                .timeout(self.fetch_timeout())
                .await,
        );

        record_fetch_duration(now.elapsed(), subgraph_name, batch_size);
        result.inspect_err(|err| record_fetch_error(err, subgraph_name))
    }

    #[doc(hidden)]
    async fn internal_invalidate_by_subgraph(&self, subgraph_name: &str) -> StorageResult<u64>;

    /// Invalidate all data associated with `subgraph_names`. Command will be timed out after
    /// `self.invalidate_timeout()`.
    async fn invalidate_by_subgraph(
        &self,
        subgraph_name: &str,
        invalidation_kind: InvalidationKind,
    ) -> StorageResult<u64> {
        let now = Instant::now();
        let result = flatten_storage_error(
            self.internal_invalidate_by_subgraph(subgraph_name)
                .timeout(self.invalidate_timeout())
                .await,
        );

        record_invalidation_duration(now.elapsed(), invalidation_kind);
        result
    }

    #[doc(hidden)]
    async fn internal_invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> StorageResult<HashMap<String, u64>>;

    /// Invalidate all data associated with at least one of the `invalidation_keys` **and** at
    /// least one of the `subgraph_names`. Command will be timed out after `self.invalidate_timeout()`.
    async fn invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
        invalidation_kind: InvalidationKind,
    ) -> StorageResult<HashMap<String, u64>> {
        let now = Instant::now();
        let result = flatten_storage_error(
            self.internal_invalidate(invalidation_keys, subgraph_names)
                .timeout(self.invalidate_timeout())
                .await,
        );

        record_invalidation_duration(now.elapsed(), invalidation_kind);
        result
    }

    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    async fn truncate_namespace(&self) -> StorageResult<()>;
}

fn flatten_storage_error<V, E>(value: Result<Result<V, Error>, E>) -> Result<V, Error>
where
    E: Into<Error>,
{
    value.map_err(Into::into).flatten()
}
