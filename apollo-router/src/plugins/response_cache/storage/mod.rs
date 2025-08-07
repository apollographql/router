use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::time::Duration;

use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::cache_control::CacheControl;
use crate::plugins::response_cache::storage::postgres::BatchDocument;
use crate::plugins::response_cache::storage::postgres::CacheEntry;

pub(crate) mod postgres;
mod redis;

#[derive(Debug)]
pub(super) enum Error {
    Postgres(sqlx::Error),
}

impl Error {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Error::Postgres(err) => err.code(),
        }
    }

    pub(super) fn is_row_not_found(&self) -> bool {
        match self {
            Error::Postgres(err) => matches!(err, sqlx::Error::RowNotFound),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Postgres(err) => f.write_str(&err.to_string()),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Error::Postgres(err)
    }
}

type StorageResult<T> = Result<T, Error>;

pub(super) trait CacheStorage {
    async fn insert(
        &self,
        cache_key: &str,
        expire: Duration,
        invalidation_keys: Vec<String>,
        value: serde_json_bytes::Value,
        control: CacheControl,
        subgraph_name: &str,
    ) -> StorageResult<()>;

    async fn insert_in_batch(
        &self,
        batch_docs: Vec<BatchDocument>,
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

    async fn expired_data_count(&self) -> StorageResult<u64>;
}
