pub(super) mod error;
pub(super) mod postgres;

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use tokio_util::future::FutureExt;

use super::cache_control::CacheControl;

type StorageResult<T> = Result<T, error::Error>;

#[derive(Debug, Clone)]
pub(super) struct Document {
    pub(super) key: String,
    pub(super) data: serde_json_bytes::Value,
    pub(super) control: CacheControl,
    pub(super) invalidation_keys: Vec<String>,
    pub(super) expire: Duration,
}

#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    pub(super) key: String,
    pub(super) data: serde_json_bytes::Value,
    pub(super) control: CacheControl,
}

pub(super) trait CacheStorage {
    /// Timeout to apply to insert command.
    fn insert_timeout(&self) -> Duration;
    fn fetch_timeout(&self) -> Duration;
    fn invalidate_timeout(&self) -> Duration;

    #[doc(hidden)]
    async fn internal_insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()>;

    async fn insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        let now = Instant::now();
        let result = self
            .internal_insert(document, subgraph_name)
            .timeout(self.insert_timeout())
            .await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.insert",
            "Time to insert new data in cache",
            "s",
            now.elapsed().as_secs_f64(),
            "subgraph.name" = subgraph_name.to_string(),
            "kind" = "single"
        );
        result?
    }

    #[doc(hidden)]
    async fn internal_insert_in_batch(
        &self,
        documents: Vec<Document>,
        subgraph_name: &str,
    ) -> StorageResult<()>;

    async fn insert_in_batch(
        &self,
        documents: Vec<Document>,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        let batch_size = batch_size_str(documents.len());

        let now = Instant::now();
        let result = self
            .internal_insert_in_batch(documents, subgraph_name)
            .timeout(self.insert_timeout())
            .await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.insert",
            "Time to insert new data in cache",
            "s",
            now.elapsed().as_secs_f64(),
            "subgraph.name" = subgraph_name.to_string(),
            "kind" = "batch",
            "batch.size" = batch_size
        );
        result?
    }

    #[doc(hidden)]
    async fn internal_fetch(&self, cache_key: &str) -> StorageResult<CacheEntry>;

    async fn fetch(&self, cache_key: &str, subgraph_name: &str) -> StorageResult<CacheEntry> {
        let now = Instant::now();
        let result = self
            .internal_fetch(cache_key)
            .timeout(self.fetch_timeout())
            .await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.fetch",
            "Time to fetch data from cache",
            "s",
            now.elapsed().as_secs_f64(),
            "subgraph.name" = subgraph_name.to_string(),
            "kind" = "single"
        );

        result?
    }

    #[doc(hidden)]
    async fn internal_fetch_multiple(
        &self,
        cache_keys: &[&str],
    ) -> StorageResult<Vec<Option<CacheEntry>>>;

    async fn fetch_multiple(
        &self,
        cache_keys: &[&str],
        subgraph_name: &str,
    ) -> StorageResult<Vec<Option<CacheEntry>>> {
        let batch_size = batch_size_str(cache_keys.len());

        let now = Instant::now();
        let result = self
            .internal_fetch_multiple(cache_keys)
            .timeout(self.fetch_timeout())
            .await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.fetch",
            "Time to fetch data from cache",
            "s",
            now.elapsed().as_secs_f64(),
            "subgraph.name" = subgraph_name.to_string(),
            "kind" = "batch",
            "batch.size" = batch_size
        );
        result?
    }

    #[doc(hidden)]
    async fn internal_invalidate_by_subgraphs(
        &self,
        subgraph_names: Vec<String>,
    ) -> StorageResult<u64>;

    async fn invalidate_by_subgraphs(&self, subgraph_names: Vec<String>) -> StorageResult<u64> {
        let now = Instant::now();
        let result = self
            .internal_invalidate_by_subgraphs(subgraph_names)
            .timeout(self.invalidate_timeout())
            .await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.invalidation",
            "Time to get invalidate data in cache",
            "s",
            now.elapsed().as_secs_f64(),
            "kind" = "subgraph"
        );
        result?
    }

    #[doc(hidden)]
    async fn internal_invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> StorageResult<HashMap<String, u64>>;

    async fn invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> StorageResult<HashMap<String, u64>> {
        let now = Instant::now();
        let result = self
            .internal_invalidate(invalidation_keys, subgraph_names)
            .timeout(self.invalidate_timeout())
            .await;

        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.invalidation",
            "Time to get invalidate data in cache",
            "s",
            now.elapsed().as_secs_f64(),
            "kind" = "invalidation_keys"
        );
        result?
    }

    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    async fn truncate_namespace(&self) -> StorageResult<()>;
}

fn batch_size_str(batch_size: usize) -> &'static str {
    if batch_size <= 10 {
        "1-10"
    } else if batch_size <= 20 {
        "11-20"
    } else if batch_size <= 50 {
        "21-50"
    } else {
        "50+"
    }
}
