#![allow(clippy::disallowed_names)]

use async_trait::async_trait;
use fred::{
  prelude::*,
  types::{
    config::{DynamicPoolConfig, PoolScale},
    stats::PoolStats,
  },
};
use log::{debug, warn};
use parking_lot::Mutex;
use std::{ops::Add, sync::Arc, time::Duration};
use tokio::time::sleep;

/// Sample usage stats and conditionally scale every 10 seconds.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// Implements a scaling policy that adds a connection if the average network latency increases
/// by more than 50% or the pool sent more than 1000 commands, or removes a connection if less than
/// 100 commands have been sent since the last sample (in this case 10 seconds).
///
/// This is probably not a good example to try with real-world traffic.
#[derive(Debug, Default)]
struct ScalePolicy {
  last_avg_latency: Mutex<Option<f64>>,
}

impl ScalePolicy {
  /// Whether latency has increased by more than 50% since the last sample.
  pub fn latency_increased(&self, usage: &PoolStats) -> bool {
    let latency_avg_sum = usage
      .clients
      .iter()
      .fold(0.0, |sum, (_, stats)| sum + stats.network_latency.avg);
    let latency_avg = latency_avg_sum / usage.clients.len() as f64;

    let mut guard = self.last_avg_latency.lock();
    let should_scale_up = if let Some(old_latency_avg) = guard.as_ref() {
      *old_latency_avg > 0.0 && latency_avg > old_latency_avg * 1.5
    } else {
      false
    };

    guard.replace(latency_avg);
    should_scale_up
  }

  /// Whether more than 1000 commands have been sent since the last sample.
  pub fn sent_gt_1000_commands(&self, stats: &PoolStats) -> bool {
    let total_commands = stats
      .clients
      .iter()
      .fold(0, |sum, (_, stats)| sum + stats.network_latency.samples);

    total_commands >= 1000
  }

  /// Whether less than 100 commands have been sent since the last sample.
  pub fn sent_lt_100_commands(&self, stats: &PoolStats) -> bool {
    let total_commands = stats
      .clients
      .iter()
      .fold(0, |sum, (_, stats)| sum + stats.network_latency.samples);

    total_commands < 100
  }
}

#[async_trait]
impl PoolScale for ScalePolicy {
  fn scale(&self, usage: PoolStats) -> i64 {
    if self.latency_increased(&usage) || self.sent_gt_1000_commands(&usage) {
      1
    } else if self.sent_lt_100_commands(&usage) {
      -1
    } else {
      0
    }
  }

  async fn on_added(&self, clients: Vec<Client>) {
    debug!("Added {} client(s)", clients.len());
    for client in clients.into_iter() {
      // set up event listeners for any clients added to the pool
      client.on_error(|(error, server)| async move {
        println!("Client {:?} disconnected with error: {:?}", server, error);
        Ok(())
      });
    }
  }

  async fn on_failure(&self, error: Error) {
    warn!("Failed to add client to pool: {:?}", error);
  }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
  pretty_env_logger::init();
  let config = Config::from_url("redis://foo:bar@redis-main:6379")?;
  let pool_config = DynamicPoolConfig {
    min_clients:                      2,
    max_clients:                      20,
    // remove connections idle for more than 5 min
    max_idle_time:                    Duration::from_secs(5 * 60),
    scale:                            Arc::new(ScalePolicy::default()),
    #[cfg(feature = "dns")]
    resolver:                         None,
  };
  let pool = Builder::from_config(config)
    .set_pool_config(pool_config)
    .build_dynamic_pool()?;
  pool.init().await?;
  // start a task that samples metrics and periodically calls the `scale` fn above
  pool.start_scale_task(SAMPLE_INTERVAL);

  // send 1001 commands so the pool adds a client
  for _ in 0 .. 1001 {
    let _: () = pool.next().incr("foo").await?;
  }
  // wait a few sec for the scale task to sample metrics and add a client
  sleep(SAMPLE_INTERVAL.add(Duration::from_secs(1))).await;
  assert_eq!(pool.size(), 3);

  // send less than 100 commands
  for _ in 0 .. 42 {
    let _: () = pool.next().incr("foo").await?;
  }
  // wait a few sec for the scale task to sample metrics and remove a client
  sleep(SAMPLE_INTERVAL.add(Duration::from_secs(1))).await;
  assert_eq!(pool.size(), 2);

  // or manually scale the pool
  pool.scale(1).await;
  pool.scale(-1).await;

  pool.quit().await?;
  Ok(())
}
