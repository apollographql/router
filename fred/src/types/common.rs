pub use crate::protocol::{
  hashers::ClusterHash,
  types::{Message, MessageKind},
};
use crate::{
  error::{Error, ErrorKind},
  types::{Key, Value},
  utils,
};
use bytes_utils::Str;
use std::{convert::TryFrom, fmt, time::Duration};

use crate::prelude::Server;
#[cfg(feature = "i-memory")]
use crate::utils::convert_or_default;
#[cfg(feature = "i-memory")]
use std::collections::HashMap;

/// Arguments passed to the SHUTDOWN command.
///
/// <https://redis.io/commands/shutdown>
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShutdownFlags {
  Save,
  NoSave,
}

impl ShutdownFlags {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ShutdownFlags::Save => "SAVE",
      ShutdownFlags::NoSave => "NOSAVE",
    })
  }
}

/// The state of the underlying connection to the Redis server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientState {
  Disconnected,
  Disconnecting,
  Connected,
  Connecting,
}

impl ClientState {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ClientState::Connecting => "Connecting",
      ClientState::Connected => "Connected",
      ClientState::Disconnecting => "Disconnecting",
      ClientState::Disconnected => "Disconnected",
    })
  }
}

impl fmt::Display for ClientState {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}", self.to_str())
  }
}
/// An enum describing the possible ways in which a Redis cluster can change state.
///
/// See [on_cluster_change](crate::interfaces::EventInterface::on_cluster_change) for more information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterStateChange {
  /// A node was added to the cluster.
  ///
  /// This implies that hash slots were also probably rebalanced.
  Add(Server),
  /// A node was removed from the cluster.
  ///
  /// This implies that hash slots were also probably rebalanced.
  Remove(Server),
  /// Hash slots were rebalanced across the cluster and/or local routing state was updated.
  Rebalance,
}

/// Arguments to the CLIENT UNBLOCK command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientUnblockFlag {
  Timeout,
  Error,
}

impl ClientUnblockFlag {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ClientUnblockFlag::Timeout => "TIMEOUT",
      ClientUnblockFlag::Error => "ERROR",
    })
  }
}

/// An event on the publish-subscribe interface describing a keyspace notification.
///
/// <https://redis.io/topics/notifications>
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct KeyspaceEvent {
  pub db:        u8,
  pub operation: String,
  pub key:       Key,
}

/// Options for the [info](https://redis.io/commands/info) command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InfoKind {
  Default,
  All,
  Keyspace,
  Cluster,
  CommandStats,
  Cpu,
  Replication,
  Stats,
  Persistence,
  Memory,
  Clients,
  Server,
}

impl InfoKind {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      InfoKind::Default => "default",
      InfoKind::All => "all",
      InfoKind::Keyspace => "keyspace",
      InfoKind::Cluster => "cluster",
      InfoKind::CommandStats => "commandstats",
      InfoKind::Cpu => "cpu",
      InfoKind::Replication => "replication",
      InfoKind::Stats => "stats",
      InfoKind::Persistence => "persistence",
      InfoKind::Memory => "memory",
      InfoKind::Clients => "clients",
      InfoKind::Server => "server",
    })
  }
}

/// Configuration for custom redis commands, primarily used for interacting with third party modules or extensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomCommand {
  /// The command name, sent directly to the server.
  pub cmd:          Str,
  /// The cluster hashing policy to use, if any.
  ///
  /// Cluster clients will use the default policy if not provided.
  pub cluster_hash: ClusterHash,
  /// Whether the command should block the connection while waiting on a response.
  pub blocking:     bool,
}

impl CustomCommand {
  /// Create a new custom command.
  ///
  /// See the [custom](crate::interfaces::ClientLike::custom) command for more information.
  pub fn new<C, H>(cmd: C, cluster_hash: H, blocking: bool) -> Self
  where
    C: Into<Str>,
    H: Into<ClusterHash>,
  {
    CustomCommand {
      cmd: cmd.into(),
      cluster_hash: cluster_hash.into(),
      blocking,
    }
  }

  /// Create a new custom command specified by a `&'static str`.
  pub fn new_static<H>(cmd: &'static str, cluster_hash: H, blocking: bool) -> Self
  where
    H: Into<ClusterHash>,
  {
    CustomCommand {
      cmd: utils::static_str(cmd),
      cluster_hash: cluster_hash.into(),
      blocking,
    }
  }
}

/// Options for the [set](https://redis.io/commands/set) command.
///
/// <https://redis.io/commands/set>
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SetOptions {
  NX,
  XX,
}

impl SetOptions {
  #[allow(dead_code)]
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      SetOptions::NX => "NX",
      SetOptions::XX => "XX",
    })
  }
}

/// Options for certain expiration commands (`PEXPIRE`, etc).
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ExpireOptions {
  NX,
  XX,
  GT,
  LT,
}

impl ExpireOptions {
  #[allow(dead_code)]
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ExpireOptions::NX => "NX",
      ExpireOptions::XX => "XX",
      ExpireOptions::GT => "GT",
      ExpireOptions::LT => "LT",
    })
  }
}

/// Expiration options for the [set](https://redis.io/commands/set) command.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Expiration {
  /// Expiration in seconds.
  EX(i64),
  /// Expiration in milliseconds.
  PX(i64),
  /// Expiration time, in seconds.
  EXAT(i64),
  /// Expiration time, in milliseconds.
  PXAT(i64),
  /// Do not reset the TTL.
  KEEPTTL,
}

impl Expiration {
  #[allow(dead_code)]
  pub(crate) fn into_args(self) -> (Str, Option<i64>) {
    let (prefix, value) = match self {
      Expiration::EX(i) => ("EX", Some(i)),
      Expiration::PX(i) => ("PX", Some(i)),
      Expiration::EXAT(i) => ("EXAT", Some(i)),
      Expiration::PXAT(i) => ("PXAT", Some(i)),
      Expiration::KEEPTTL => ("KEEPTTL", None),
    };

    (utils::static_str(prefix), value)
  }
}

/// The parsed result of the MEMORY STATS command for a specific database.
///
/// <https://redis.io/commands/memory-stats>
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
pub struct DatabaseMemoryStats {
  pub overhead_hashtable_main:         u64,
  pub overhead_hashtable_expires:      u64,
  pub overhead_hashtable_slot_to_keys: u64,
}

#[cfg(feature = "i-memory")]
impl Default for DatabaseMemoryStats {
  fn default() -> Self {
    DatabaseMemoryStats {
      overhead_hashtable_expires:      0,
      overhead_hashtable_main:         0,
      overhead_hashtable_slot_to_keys: 0,
    }
  }
}

#[cfg(feature = "i-memory")]
fn parse_database_memory_stat(stats: &mut DatabaseMemoryStats, key: &str, value: Value) {
  match key {
    "overhead.hashtable.main" => stats.overhead_hashtable_main = convert_or_default(value),
    "overhead.hashtable.expires" => stats.overhead_hashtable_expires = convert_or_default(value),
    "overhead.hashtable.slot-to-keys" => stats.overhead_hashtable_slot_to_keys = convert_or_default(value),
    _ => {},
  };
}

#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl TryFrom<Value> for DatabaseMemoryStats {
  type Error = Error;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    let values: HashMap<Str, Value> = value.convert()?;
    let mut out = DatabaseMemoryStats::default();

    for (key, value) in values.into_iter() {
      parse_database_memory_stat(&mut out, &key, value);
    }
    Ok(out)
  }
}

/// The parsed result of the MEMORY STATS command.
///
/// <https://redis.io/commands/memory-stats>
#[derive(Clone, Debug)]
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
pub struct MemoryStats {
  pub peak_allocated:                u64,
  pub total_allocated:               u64,
  pub startup_allocated:             u64,
  pub replication_backlog:           u64,
  pub clients_slaves:                u64,
  pub clients_normal:                u64,
  pub aof_buffer:                    u64,
  pub lua_caches:                    u64,
  pub overhead_total:                u64,
  pub keys_count:                    u64,
  pub keys_bytes_per_key:            u64,
  pub dataset_bytes:                 u64,
  pub dataset_percentage:            f64,
  pub peak_percentage:               f64,
  pub fragmentation:                 f64,
  pub fragmentation_bytes:           u64,
  pub rss_overhead_ratio:            f64,
  pub rss_overhead_bytes:            u64,
  pub allocator_allocated:           u64,
  pub allocator_active:              u64,
  pub allocator_resident:            u64,
  pub allocator_fragmentation_ratio: f64,
  pub allocator_fragmentation_bytes: u64,
  pub allocator_rss_ratio:           f64,
  pub allocator_rss_bytes:           u64,
  pub db:                            HashMap<u16, DatabaseMemoryStats>,
}

#[cfg(feature = "i-memory")]
impl Default for MemoryStats {
  fn default() -> Self {
    MemoryStats {
      peak_allocated:                0,
      total_allocated:               0,
      startup_allocated:             0,
      replication_backlog:           0,
      clients_normal:                0,
      clients_slaves:                0,
      aof_buffer:                    0,
      lua_caches:                    0,
      overhead_total:                0,
      keys_count:                    0,
      keys_bytes_per_key:            0,
      dataset_bytes:                 0,
      dataset_percentage:            0.0,
      peak_percentage:               0.0,
      fragmentation:                 0.0,
      fragmentation_bytes:           0,
      rss_overhead_ratio:            0.0,
      rss_overhead_bytes:            0,
      allocator_allocated:           0,
      allocator_active:              0,
      allocator_resident:            0,
      allocator_fragmentation_ratio: 0.0,
      allocator_fragmentation_bytes: 0,
      allocator_rss_bytes:           0,
      allocator_rss_ratio:           0.0,
      db:                            HashMap::new(),
    }
  }
}
#[cfg(feature = "i-memory")]
impl PartialEq for MemoryStats {
  fn eq(&self, other: &Self) -> bool {
    self.peak_allocated == other.peak_allocated
      && self.total_allocated == other.total_allocated
      && self.startup_allocated == other.startup_allocated
      && self.replication_backlog == other.replication_backlog
      && self.clients_normal == other.clients_normal
      && self.clients_slaves == other.clients_slaves
      && self.aof_buffer == other.aof_buffer
      && self.lua_caches == other.lua_caches
      && self.overhead_total == other.overhead_total
      && self.keys_count == other.keys_count
      && self.keys_bytes_per_key == other.keys_bytes_per_key
      && self.dataset_bytes == other.dataset_bytes
      && utils::f64_eq(self.dataset_percentage, other.dataset_percentage)
      && utils::f64_eq(self.peak_percentage, other.peak_percentage)
      && utils::f64_eq(self.fragmentation, other.fragmentation)
      && self.fragmentation_bytes == other.fragmentation_bytes
      && utils::f64_eq(self.rss_overhead_ratio, other.rss_overhead_ratio)
      && self.rss_overhead_bytes == other.rss_overhead_bytes
      && self.allocator_allocated == other.allocator_allocated
      && self.allocator_active == other.allocator_active
      && self.allocator_resident == other.allocator_resident
      && utils::f64_eq(self.allocator_fragmentation_ratio, other.allocator_fragmentation_ratio)
      && self.allocator_fragmentation_bytes == other.allocator_fragmentation_bytes
      && self.allocator_rss_bytes == other.allocator_rss_bytes
      && utils::f64_eq(self.allocator_rss_ratio, other.allocator_rss_ratio)
      && self.db == other.db
  }
}

#[cfg(feature = "i-memory")]
impl Eq for MemoryStats {}

#[cfg(feature = "i-memory")]
fn parse_memory_stat_field(stats: &mut MemoryStats, key: &str, value: Value) {
  match key {
    "peak.allocated" => stats.peak_allocated = convert_or_default(value),
    "total.allocated" => stats.total_allocated = convert_or_default(value),
    "startup.allocated" => stats.startup_allocated = convert_or_default(value),
    "replication.backlog" => stats.replication_backlog = convert_or_default(value),
    "clients.slaves" => stats.clients_slaves = convert_or_default(value),
    "clients.normal" => stats.clients_normal = convert_or_default(value),
    "aof.buffer" => stats.aof_buffer = convert_or_default(value),
    "lua.caches" => stats.lua_caches = convert_or_default(value),
    "overhead.total" => stats.overhead_total = convert_or_default(value),
    "keys.count" => stats.keys_count = convert_or_default(value),
    "keys.bytes-per-key" => stats.keys_bytes_per_key = convert_or_default(value),
    "dataset.bytes" => stats.dataset_bytes = convert_or_default(value),
    "dataset.percentage" => stats.dataset_percentage = convert_or_default(value),
    "peak.percentage" => stats.peak_percentage = convert_or_default(value),
    "allocator.allocated" => stats.allocator_allocated = convert_or_default(value),
    "allocator.active" => stats.allocator_active = convert_or_default(value),
    "allocator.resident" => stats.allocator_resident = convert_or_default(value),
    "allocator-fragmentation.ratio" => stats.allocator_fragmentation_ratio = convert_or_default(value),
    "allocator-fragmentation.bytes" => stats.allocator_fragmentation_bytes = convert_or_default(value),
    "allocator-rss.ratio" => stats.allocator_rss_ratio = convert_or_default(value),
    "allocator-rss.bytes" => stats.allocator_rss_bytes = convert_or_default(value),
    "rss-overhead.ratio" => stats.rss_overhead_ratio = convert_or_default(value),
    "rss-overhead.bytes" => stats.rss_overhead_bytes = convert_or_default(value),
    "fragmentation" => stats.fragmentation = convert_or_default(value),
    "fragmentation.bytes" => stats.fragmentation_bytes = convert_or_default(value),
    _ => {
      if key.starts_with("db.") {
        let db = match key.split('.').last().and_then(|v| v.parse::<u16>().ok()) {
          Some(db) => db,
          None => return,
        };
        let parsed: DatabaseMemoryStats = match value.convert().ok() {
          Some(db) => db,
          None => return,
        };

        stats.db.insert(db, parsed);
      }
    },
  }
}

#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl TryFrom<Value> for MemoryStats {
  type Error = Error;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    let values: HashMap<Str, Value> = value.convert()?;
    let mut out = MemoryStats::default();

    for (key, value) in values.into_iter() {
      parse_memory_stat_field(&mut out, &key, value);
    }
    Ok(out)
  }
}

/// The output of an entry in the slow queries log.
///
/// <https://redis.io/commands/slowlog#output-format>
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlowlogEntry {
  pub id:        i64,
  pub timestamp: i64,
  pub duration:  Duration,
  pub args:      Vec<Value>,
  pub ip:        Option<Str>,
  pub name:      Option<Str>,
}

impl TryFrom<Value> for SlowlogEntry {
  type Error = Error;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    if let Value::Array(values) = value {
      if values.len() < 4 {
        return Err(Error::new(ErrorKind::Protocol, "Expected at least 4 response values."));
      }

      let id = values[0]
        .as_i64()
        .ok_or(Error::new(ErrorKind::Protocol, "Expected integer ID."))?;
      let timestamp = values[1]
        .as_i64()
        .ok_or(Error::new(ErrorKind::Protocol, "Expected integer timestamp."))?;
      let duration = values[2]
        .as_u64()
        .map(Duration::from_micros)
        .ok_or(Error::new(ErrorKind::Protocol, "Expected integer duration."))?;
      let args = values[3].clone().into_multiple_values();

      let (ip, name) = if values.len() == 6 {
        let ip = values[4]
          .as_bytes_str()
          .ok_or(Error::new(ErrorKind::Protocol, "Expected IP address string."))?;
        let name = values[5]
          .as_bytes_str()
          .ok_or(Error::new(ErrorKind::Protocol, "Expected client name string."))?;

        (Some(ip), Some(name))
      } else {
        (None, None)
      };

      Ok(SlowlogEntry {
        id,
        timestamp,
        duration,
        args,
        ip,
        name,
      })
    } else {
      Err(Error::new_parse("Expected array."))
    }
  }
}

/// Arguments for the `SENTINEL SIMULATE-FAILURE` command.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(feature = "sentinel-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
pub enum SentinelFailureKind {
  CrashAfterElection,
  CrashAfterPromotion,
  Help,
}

#[cfg(feature = "sentinel-client")]
impl SentinelFailureKind {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match self {
      SentinelFailureKind::CrashAfterElection => "crash-after-election",
      SentinelFailureKind::CrashAfterPromotion => "crash-after-promotion",
      SentinelFailureKind::Help => "help",
    })
  }
}

/// The sort order for redis commands that take or return a sorted list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SortOrder {
  Asc,
  Desc,
}

impl SortOrder {
  #[allow(dead_code)]
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      SortOrder::Asc => "ASC",
      SortOrder::Desc => "DESC",
    })
  }
}
