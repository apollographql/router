use std::ops::Deref;
use std::sync::Arc;

use fred::interfaces::ClientLike;
use fred::prelude::Pool as RedisPool;
use tokio::task::AbortHandle;

use super::Error;
use super::metrics::RedisMetricsCollector;

/// `DropSafeRedisPool` is a wrapper for `fred::prelude::RedisPool` which closes the pool's Redis
/// connections when it is dropped.
//
// Dev notes:
// * the inner `RedisPool` must be wrapped in an `Arc` because closing the connections happens
//   in a spawned async task.
// * why not just implement this within `Drop` for `RedisCacheStorage`? Because `RedisCacheStorage`
//   is cloned frequently throughout the router, and we don't want to close the connections
//   when each clone is dropped, only when the last instance is dropped.
pub(super) struct DropSafeRedisPool {
    pub(super) pool: Arc<RedisPool>,
    pub(super) caller: &'static str,
    pub(super) heartbeat_abort_handle: AbortHandle,
    // Metrics collector handles its own abort and gauges
    pub(super) _metrics_collector: RedisMetricsCollector,
}

impl Deref for DropSafeRedisPool {
    type Target = RedisPool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

impl Drop for DropSafeRedisPool {
    fn drop(&mut self) {
        let inner = self.pool.clone();
        let caller = self.caller;
        tokio::spawn(async move {
            let result = inner.quit().await.map_err(Error::from);
            if let Err(err) = result {
                tracing::warn!("Caught error while closing unused Redis connections: {err:?}");
                super::error::record(&err, caller);
            }
        });

        // Metrics collector will be dropped automatically and its Drop impl will abort the task
        self.heartbeat_abort_handle.abort();
    }
}
