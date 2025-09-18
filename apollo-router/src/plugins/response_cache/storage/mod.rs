use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::time::Duration;
use std::time::Instant;

use tokio::task::JoinError;
use tokio::time::timeout;

use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::cache_control::CacheControl;

pub(crate) mod redis;

#[derive(Debug)]
pub(super) enum Error {
    Redis(fred::error::Error),
    Serialize(serde_json::Error),
    Timeout,
    JoinError(JoinError),
}

impl Error {
    pub(super) fn is_row_not_found(&self) -> bool {
        match self {
            Error::Redis(err) => err.is_not_found(),
            Error::Serialize(_) => false,
            Error::JoinError(_) => false,
            Error::Timeout => false,
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Redis(err) => f.write_str(&err.to_string()),
            Error::Serialize(err) => f.write_str(&err.to_string()),
            Error::JoinError(err) => f.write_str(&err.to_string()),
            Error::Timeout => f.write_str("TIMED_OUT"),
        }
    }
}

impl From<fred::error::Error> for Error {
    fn from(err: fred::error::Error) -> Self {
        Error::Redis(err)
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

impl From<JoinError> for Error {
    fn from(err: JoinError) -> Self {
        Error::JoinError(err)
    }
}

impl ErrorCode for Error {
    fn code(&self) -> &'static str {
        match self {
            Error::Redis(err) => err.kind().to_str(),
            Error::Serialize(_) => "serialize // TODO",
            Error::JoinError(_) => "join_error // TODO",
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

    async fn _insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()>;
    async fn insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()> {
        let now = Instant::now();

        let result = timeout(
            self.timeout_duration(),
            self._insert(document, subgraph_name),
        )
        .await;

        let elapsed = now.elapsed().as_secs_f64();
        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.storage.insert",
            "Time to insert new data in cache",
            "s",
            elapsed,
            "kind" = "single"
        );
        result?
    }

    async fn _insert_in_batch(
        &self,
        batch_docs: Documents,
        subgraph_name: &str,
    ) -> StorageResult<()>;
    async fn insert_in_batch(
        &self,
        batch_docs: Documents,
        subgraph_name: &str,
    ) -> StorageResult<()> {
        let now = Instant::now();
        let result = timeout(
            self.timeout_duration(),
            self._insert_in_batch(batch_docs, subgraph_name),
        )
        .await;

        let elapsed = now.elapsed().as_secs_f64();
        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.storage.insert",
            "Time to insert new data in cache",
            "s",
            elapsed,
            "kind" = "batch"
        );
        result?
    }

    async fn _get(&self, cache_key: &str) -> StorageResult<CacheEntry>;
    async fn get(&self, cache_key: &str) -> StorageResult<CacheEntry> {
        let now = Instant::now();
        let result = timeout(self.timeout_duration(), self._get(cache_key)).await;

        let elapsed = now.elapsed().as_secs_f64();
        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.storage.get",
            "Time to get new data from cache",
            "s",
            elapsed,
            "kind" = "single"
        );
        result?
    }

    async fn _get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>>;
    async fn get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>> {
        let now = Instant::now();
        let result = timeout(self.timeout_duration(), self._get_multiple(cache_keys)).await;

        let elapsed = now.elapsed().as_secs_f64();
        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.storage.get",
            "Time to get new data from cache",
            "s",
            elapsed,
            "kind" = "batch"
        );
        result?
    }

    async fn _invalidate_by_subgraphs(&self, subgraph_names: Vec<String>) -> StorageResult<u64>;
    async fn invalidate_by_subgraphs(&self, subgraph_names: Vec<String>) -> StorageResult<u64> {
        let now = Instant::now();
        let result = timeout(
            self.timeout_duration(),
            self._invalidate_by_subgraphs(subgraph_names),
        )
        .await;

        let elapsed = now.elapsed().as_secs_f64();
        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.storage.invalidate",
            "Time to get invalidate data in cache",
            "s",
            elapsed,
            "kind" = "subgraphs"
        );
        result?
    }

    async fn _invalidate(
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
        let result = timeout(
            self.timeout_duration(),
            self._invalidate(invalidation_keys, subgraph_names),
        )
        .await;

        let elapsed = now.elapsed().as_secs_f64();
        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.storage.invalidate",
            "Time to get invalidate data in cache",
            "s",
            elapsed,
            "kind" = "specific"
        );
        result?
    }

    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    async fn truncate_namespace(&self) -> StorageResult<()>;
}
