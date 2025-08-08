use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::time::Duration;

use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::cache_control::CacheControl;

pub(crate) mod redis;

#[derive(Debug)]
pub(super) enum Error {
    Redis(fred::error::Error),
    Serialize(serde_json::Error),
}

impl Error {
    pub(super) fn is_row_not_found(&self) -> bool {
        match self {
            Error::Redis(err) => err.is_not_found(),
            Error::Serialize(_) => false,
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Redis(err) => f.write_str(&err.to_string()),
            Error::Serialize(err) => f.write_str(&err.to_string()),
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

impl ErrorCode for Error {
    fn code(&self) -> &'static str {
        match self {
            Error::Redis(err) => err.kind().to_str(),
            Error::Serialize(_) => "serialize // TODO",
        }
    }
}

impl std::error::Error for Error {}

type StorageResult<T> = Result<T, Error>;

#[derive(Debug, Clone)]
pub(crate) struct Document {
    pub(crate) cache_key: String,
    pub(crate) data: String,
    pub(crate) control: String,
    pub(crate) invalidation_keys: Vec<String>,
    pub(crate) expire: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct CacheEntry {
    pub(crate) cache_key: String,
    pub(crate) data: serde_json_bytes::Value,
    pub(crate) control: CacheControl,
}

pub(super) trait CacheStorage {
    async fn insert(&self, document: Document, subgraph_name: &str) -> StorageResult<()>;

    async fn insert_in_batch(
        &self,
        batch_docs: Vec<Document>,
        subgraph_name: &str,
    ) -> StorageResult<()>;

    async fn get(&self, cache_key: &str) -> StorageResult<CacheEntry>;

    async fn get_multiple(&self, cache_keys: &[&str]) -> StorageResult<Vec<Option<CacheEntry>>>;

    async fn invalidate_by_subgraphs(&self, subgraph_names: Vec<String>) -> StorageResult<u64>;

    async fn invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> StorageResult<HashMap<String, u64>>;

    #[cfg(test)]
    #[allow(dead_code)] // only used in very specific tests that don't match cfg(test)
    async fn truncate_namespace(&self) -> StorageResult<()>;
}
