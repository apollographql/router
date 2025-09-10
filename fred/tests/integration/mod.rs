#[macro_use]
pub mod utils;
#[cfg(feature = "i-acl")]
mod acl;
#[cfg(feature = "i-client")]
mod client;
#[cfg(feature = "i-cluster")]
mod cluster;
//#[cfg(feature = "i-cluster")]
// pub mod docker;
#[cfg(feature = "i-geo")]
mod geo;
#[cfg(feature = "i-hashes")]
mod hashes;
#[cfg(feature = "i-hyperloglog")]
mod hyperloglog;
#[cfg(feature = "i-keys")]
mod keys;
#[cfg(feature = "i-lists")]
mod lists;
#[cfg(feature = "i-scripts")]
mod lua;
#[cfg(feature = "i-memory")]
mod memory;
#[cfg(feature = "transactions")]
mod multi;
mod other;
mod pool;
#[cfg(feature = "i-pubsub")]
mod pubsub;
#[cfg(feature = "i-redis-json")]
mod redis_json;
#[cfg(feature = "i-redisearch")]
mod redisearch;
mod scanning;
#[cfg(feature = "i-server")]
mod server;
#[cfg(feature = "i-sets")]
mod sets;
#[cfg(feature = "i-slowlog")]
mod slowlog;
#[cfg(feature = "i-sorted-sets")]
mod sorted_sets;
#[cfg(feature = "i-streams")]
mod streams;
#[cfg(feature = "i-time-series")]
mod timeseries;
#[cfg(feature = "i-tracking")]
mod tracking;

#[cfg(not(feature = "mocks"))]
pub mod centralized;
#[cfg(not(feature = "mocks"))]
pub mod clustered;

mod macro_tests {
  use fred::{cmd, types::ClusterHash};
  use socket2::TcpKeepalive;

  #[test]
  fn should_use_cmd_macro() {
    let command = cmd!("GET");
    assert_eq!(command.cmd, "GET");
    assert_eq!(command.cluster_hash, ClusterHash::FirstKey);
    assert!(!command.blocking);
    let command = cmd!("GET", blocking: true);
    assert_eq!(command.cmd, "GET");
    assert_eq!(command.cluster_hash, ClusterHash::FirstKey);
    assert!(command.blocking);
    let command = cmd!("GET", hash: ClusterHash::FirstValue);
    assert_eq!(command.cmd, "GET");
    assert_eq!(command.cluster_hash, ClusterHash::FirstValue);
    assert!(!command.blocking);
    let command = cmd!("GET", hash: ClusterHash::FirstValue, blocking: true);
    assert_eq!(command.cmd, "GET");
    assert_eq!(command.cluster_hash, ClusterHash::FirstValue);
    assert!(command.blocking);
  }
}
