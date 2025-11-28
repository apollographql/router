use std::time::Duration;

use tokio::sync::mpsc::error::TrySendError;

use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::invalidation::InvalidationKind;
use crate::plugins::response_cache::storage;

pub(crate) const CACHE_INFO_SUBGRAPH_CONTEXT_KEY: &str =
    "apollo::router::response_cache::cache_info_subgraph";

pub(crate) struct CacheMetricContextKey(String);

impl CacheMetricContextKey {
    pub(crate) fn new(subgraph_name: String) -> Self {
        Self(subgraph_name)
    }
}

impl From<CacheMetricContextKey> for String {
    fn from(val: CacheMetricContextKey) -> Self {
        format!("{CACHE_INFO_SUBGRAPH_CONTEXT_KEY}_{}", val.0)
    }
}

pub(super) fn record_fetch_error(error: &storage::Error, subgraph_name: &str) {
    record_fetch_errors(error, subgraph_name, 1)
}

pub(super) fn record_fetch_errors(error: &storage::Error, subgraph_name: &str, count: u64) {
    if error.is_row_not_found() {
        return;
    }

    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.fetch.error",
        "Errors when fetching data from cache",
        "{error}",
        count,
        "subgraph.name" = subgraph_name.to_string(),
        "code" = error.code()
    );
    tracing::debug!(error = %error, "unable to fetch data from response cache");
}

pub(super) fn record_fetch_duration(duration: Duration, subgraph_name: &str, batch_size: usize) {
    f64_histogram_with_unit!(
        "apollo.router.operations.response_cache.fetch",
        "Time to fetch data from cache",
        "s",
        duration.as_secs_f64(),
        "subgraph.name" = subgraph_name.to_string(),
        "batch.size" = batch_size_str(batch_size)
    );
}

pub(super) fn record_insert_error(error: &storage::Error, subgraph_name: &str) {
    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.insert.error",
        "Errors when inserting data in cache",
        "{error}",
        1,
        "subgraph.name" = subgraph_name.to_string(),
        "code" = error.code()
    );
    tracing::debug!(error = %error, "unable to insert data in response cache");
}

pub(super) fn record_insert_duration(duration: Duration, subgraph_name: &str, batch_size: usize) {
    f64_histogram_with_unit!(
        "apollo.router.operations.response_cache.insert",
        "Time to insert new data in cache",
        "s",
        duration.as_secs_f64(),
        "subgraph.name" = subgraph_name.to_string(),
        "batch.size" = batch_size_str(batch_size)
    );
}

pub(super) fn record_maintenance_success(entries: u64) {
    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.maintenance.removed_cache_tag_entries",
        "Counter for removed items",
        "{entry}",
        entries
    );
}

pub(super) fn record_maintenance_error(error: &storage::Error) {
    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.maintenance.error",
        "Errors while removing expired entries from cache tag set",
        "{error}",
        1,
        "code" = error.code()
    );
    tracing::debug!(error = %error, "unable to perform maintenance on cache tag set in response cache");
}

pub(super) fn record_maintenance_duration(duration: Duration) {
    f64_histogram_with_unit!(
        "apollo.router.operations.response_cache.maintenance",
        "Time to remove expired entries from cache tag set",
        "s",
        duration.as_secs_f64()
    );
}

pub(super) fn record_maintenance_queue_error<T>(error: &TrySendError<T>) {
    let kind = match error {
        TrySendError::Closed(_) => "channel closed",
        TrySendError::Full(_) => "channel full",
    };
    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.maintenance.queue.error",
        "Error while sending cache tag to maintenance queue",
        "{error}",
        1,
        "error" = kind
    );
}

pub(super) fn record_invalidation_duration(
    duration: Duration,
    invalidation_kind: InvalidationKind,
) {
    f64_histogram_with_unit!(
        "apollo.router.operations.response_cache.invalidation",
        "Time to invalidate data in cache",
        "s",
        duration.as_secs_f64(),
        "kind" = invalidation_kind
    );
}

/// Restrict `batch_size` cardinality so that it can be used as a metric attribute.
fn batch_size_str(batch_size: usize) -> &'static str {
    if batch_size == 0 {
        "0"
    } else if batch_size <= 10 {
        "1-10"
    } else if batch_size <= 20 {
        "11-20"
    } else if batch_size <= 50 {
        "21-50"
    } else {
        "51+"
    }
}
