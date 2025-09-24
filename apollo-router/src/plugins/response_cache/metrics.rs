use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::IntervalStream;

use crate::metrics::meter_provider;
use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::storage;
use crate::plugins::response_cache::storage::postgres::Storage;

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

/// This task counts all rows in the given Postgres DB that is expired and will be removed when pg_cron will be triggered
/// parameter subgraph_name is optional and is None when the database is the global one, and Some(...) when it's a database configured for a specific subgraph
pub(super) async fn expired_data_task(
    storage: Storage,
    mut abort_signal: broadcast::Receiver<()>,
    subgraph_name: Option<String>,
) {
    let mut interval = IntervalStream::new(tokio::time::interval(std::time::Duration::from_secs(
        (storage.cleanup_interval.num_seconds().max(60) / 2) as u64,
    )));
    let expired_data_count = Arc::new(AtomicU64::new(0));
    let expired_data_count_clone = expired_data_count.clone();
    let meter = meter_provider().meter("apollo/router");
    let _gauge = meter
        .u64_observable_gauge("apollo.router.response_cache.data.expired")
        .with_description("Count of expired data entries still in database")
        .with_unit("{entry}")
        .with_callback(move |gauge| {
            let attributes = match subgraph_name.clone() {
                Some(subgraph_name) => {
                    vec![KeyValue::new(
                        "subgraph.name",
                        opentelemetry::Value::String(subgraph_name.into()),
                    )]
                }
                None => Vec::new(),
            };
            gauge.observe(
                expired_data_count_clone.load(Ordering::Relaxed),
                &attributes,
            );
        })
        .init();

    loop {
        tokio::select! {
            biased;
            _ = abort_signal.recv() => {
                break;
            }
            _ = interval.next() => {
                let exp_data = match storage.expired_data_count().await {
                    Ok(exp_data) => exp_data,
                    Err(err) => {
                        ::tracing::error!(error = ?err, "cannot get expired data count");
                        continue;
                    }
                };
                expired_data_count.store(exp_data, Ordering::Relaxed);
            }
        }
    }
}

pub(super) fn record_fetch_error(error: &storage::Error, subgraph_name: &str) {
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

/// Restrict `batch_size` cardinality so that it can be used as a metric attribute.
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
