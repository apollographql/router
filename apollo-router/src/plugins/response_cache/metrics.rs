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
    if error.is_row_not_found() {
        return;
    }

    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.fetch.error",
        "Errors when fetching data from cache",
        "{error}",
        1,
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

pub(super) fn record_maintenance_commands(deduplicated: u64, executed: u64) {
    u64_counter_with_unit!(
        "experimental.apollo.router.operations.response_cache.maintenance.commands",
        "Cache tag maintenance commands sent to or avoided by Redis deduplication within a drain cycle",
        "{command}",
        executed,
        "deduplicated" = "false"
    );
    u64_counter_with_unit!(
        "experimental.apollo.router.operations.response_cache.maintenance.commands",
        "Cache tag maintenance commands sent to or avoided by Redis deduplication within a drain cycle",
        "{command}",
        deduplicated,
        "deduplicated" = "true"
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

/// Outcome of building the CDN invalidation labels (`Cache-Tag`) header for one response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CdnTagHeaderOutcome {
    /// Nothing to report — no subgraph/type/tag labels were aggregated for this request.
    Empty,
    /// Every aggregated label fit within `max_bytes`; nothing was truncated.
    CompleteWithoutTruncation,
    /// Some labels didn't fit within `max_bytes` and were truncated, finest-grained first.
    CompleteWithTruncation,
    /// Truncation would have occurred, but `experimental_on_overflow: drop` suppressed the
    /// header entirely rather than sending a partial one.
    DroppedDueToOverflow,
}

impl CdnTagHeaderOutcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::CompleteWithoutTruncation => "complete_without_truncation",
            Self::CompleteWithTruncation => "complete_with_truncation",
            Self::DroppedDueToOverflow => "dropped_due_to_overflow",
        }
    }
}

pub(super) fn record_cdn_tag_header_outcome(outcome: CdnTagHeaderOutcome) {
    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.cdn_tag_header.outcome",
        "Outcome of building the CDN invalidation labels (Cache-Tag) header for a response",
        "{response}",
        1,
        "outcome" = outcome.as_str()
    );
}

/// Size in bytes the `Cache-Tag` header would have been had `max_bytes` not been applied —
/// recorded whenever there's at least one label to report, regardless of whether truncation
/// actually happened, so the distribution tracks headroom before truncation kicks in.
pub(super) fn record_cdn_tag_header_untruncated_size(bytes: u64) {
    u64_histogram_with_unit!(
        "apollo.router.operations.response_cache.cdn_tag_header.untruncated_size",
        "Size the CDN invalidation labels (Cache-Tag) header would have been without max_bytes truncation",
        "By",
        bytes
    );
}

pub(super) fn record_cdn_tag_header_error(reason: &'static str) {
    u64_counter_with_unit!(
        "apollo.router.operations.response_cache.cdn_tag_header.error",
        "Errors while emitting the CDN invalidation labels (Cache-Tag) header",
        "{error}",
        1,
        "reason" = reason
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
