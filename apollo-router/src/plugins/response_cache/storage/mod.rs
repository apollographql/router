pub(crate) mod postgres;

use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::time::Duration;
use std::time::Instant;

use serde_json::error::Category;
use tokio_util::future::FutureExt;

use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::cache_control::CacheControl;

#[derive(Debug)]
pub(crate) enum Error {
    Database(sqlx::Error),
    Serialize(serde_json::Error),
    Timeout,
}

impl Error {
    pub(super) fn is_row_not_found(&self) -> bool {
        match self {
            Error::Database(err) => matches!(err, &sqlx::Error::RowNotFound),
            Error::Serialize(_) => false,
            Error::Timeout => false,
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(err) => f.write_str(&err.to_string()),
            Error::Serialize(err) => f.write_str(&err.to_string()),
            Error::Timeout => f.write_str("TIMED_OUT"),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Error::Database(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Serialize(err)
    }
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Error::Timeout
    }
}

impl ErrorCode for Error {
    fn code(&self) -> &'static str {
        match self {
            Error::Database(err) => err.code(),
            Error::Serialize(err) => match err.classify() {
                Category::Io => "Serialize::IO",
                Category::Syntax => "Serialize::Syntax",
                Category::Data => "Serialize::Data",
                Category::Eof => "Serialize::EOF",
            },
            Error::Timeout => "TIMED_OUT",
        }
    }
}

impl std::error::Error for Error {}

type StorageResult<T> = Result<T, Error>;

type Documents = Vec<Document>;

#[derive(Debug, Clone)]
pub(crate) struct Document {
    pub(crate) cache_key: String,
    pub(crate) data: serde_json_bytes::Value,
    pub(crate) cache_control: CacheControl,
    pub(crate) invalidation_keys: Vec<String>,
    pub(crate) expire: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct CacheEntry {
    pub(crate) cache_key: String,
    pub(crate) data: serde_json_bytes::Value,
    pub(crate) control: CacheControl,
}

// TODO: in theory, we could have `struct Storage<S: CacheStorage>`. But the types are a huge pain.
//  Keeping this trait around for now as it provides clear expected cache behavior, but not sure if
//  that's actually good practice.
pub(super) trait CacheStorage {
    fn timeout_duration(&self) -> Duration;

    #[doc(hidden)]
    async fn internal_insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()>;

    async fn insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        let now = Instant::now();
        let result = self
            .internal_insert(document, subgraph_name)
            .timeout(self.timeout_duration())
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
        documents: Documents,
        subgraph_name: &str,
    ) -> StorageResult<()>;

    async fn insert_in_batch(
        &self,
        documents: Documents,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        let batch_size = batch_size_str(documents.len());

        let now = Instant::now();
        let result = self
            .internal_insert_in_batch(documents, subgraph_name)
            .timeout(self.timeout_duration())
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
    async fn internal_get(&self, cache_key: &str) -> StorageResult<CacheEntry>;

    async fn get(&self, cache_key: &str, subgraph_name: &str) -> StorageResult<CacheEntry> {
        let now = Instant::now();
        let result = self
            .internal_get(cache_key)
            .timeout(self.timeout_duration())
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
    async fn internal_get_multiple(
        &self,
        cache_keys: &[&str],
    ) -> StorageResult<Vec<Option<CacheEntry>>>;

    async fn get_multiple(
        &self,
        cache_keys: &[&str],
        subgraph_name: &str,
    ) -> StorageResult<Vec<Option<CacheEntry>>> {
        let batch_size = batch_size_str(cache_keys.len());

        let now = Instant::now();
        let result = self
            .internal_get_multiple(cache_keys)
            .timeout(self.timeout_duration())
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
            .timeout(self.timeout_duration())
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
            .timeout(self.timeout_duration())
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
