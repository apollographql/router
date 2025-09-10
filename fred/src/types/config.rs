pub use crate::protocol::types::Server;
use crate::{
  error::{Error, ErrorKind},
  protocol::command::Command,
  types::{ClusterHash, RespVersion},
  utils,
};
use socket2::TcpKeepalive;
use std::{cmp, fmt::Debug, time::Duration};
use url::Url;

#[cfg(feature = "mocks")]
use crate::mocks::Mocks;
#[cfg(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
))]
#[cfg_attr(
  docsrs,
  doc(cfg(any(
    feature = "enable-rustls",
    feature = "enable-native-tls",
    feature = "enable-rustls-ring"
  )))
)]
pub use crate::protocol::tls::{HostMapping, TlsConfig, TlsConnector, TlsHostMapping};
#[cfg(feature = "replicas")]
#[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
pub use crate::router::replicas::{ReplicaConfig, ReplicaFilter};
#[cfg(all(feature = "dns", feature = "dynamic-pool"))]
use crate::types::Resolve;
#[cfg(feature = "dynamic-pool")]
use crate::{clients::Client, interfaces::ClientLike, types::stats::PoolStats};
#[cfg(any(feature = "credential-provider", feature = "dynamic-pool"))]
use async_trait::async_trait;
#[cfg(feature = "dynamic-pool")]
use fred_macros::rm_send_if;
#[cfg(feature = "unix-sockets")]
use std::path::PathBuf;
#[cfg(any(feature = "mocks", feature = "credential-provider", feature = "dynamic-pool"))]
use std::sync::Arc;

/// The default amount of jitter when waiting to reconnect.
pub const DEFAULT_JITTER_MS: u32 = 100;

/// Special errors that can trigger reconnection logic, which can also retry the failing command if possible.
///
/// `MOVED`, `ASK`, and `NOAUTH` errors are handled separately by the client.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(feature = "custom-reconnect-errors")]
#[cfg_attr(docsrs, doc(cfg(feature = "custom-reconnect-errors")))]
pub enum ReconnectError {
  /// The CLUSTERDOWN prefix.
  ClusterDown,
  /// The LOADING prefix.
  Loading,
  /// The MASTERDOWN prefix.
  MasterDown,
  /// The READONLY prefix, which can happen if a primary node is switched to a replica without any connection
  /// interruption.
  ReadOnly,
  /// The MISCONF prefix.
  Misconf,
  /// The BUSY prefix.
  Busy,
  /// The NOREPLICAS prefix.
  NoReplicas,
  /// A case-sensitive prefix on an error message.
  ///
  /// See [the source](https://github.com/redis/redis/blob/fe37e4fc874a92dcf61b3b0de899ec6f674d2442/src/server.c#L1845) for examples.
  Custom(&'static str),
}

#[cfg(feature = "custom-reconnect-errors")]
impl ReconnectError {
  pub(crate) fn to_str(&self) -> &'static str {
    use ReconnectError::*;

    match self {
      ClusterDown => "CLUSTERDOWN",
      Loading => "LOADING",
      MasterDown => "MASTERDOWN",
      ReadOnly => "READONLY",
      Misconf => "MISCONF",
      Busy => "BUSY",
      NoReplicas => "NOREPLICAS",
      Custom(prefix) => prefix,
    }
  }
}

/// The type of reconnection policy to use. This will apply to every connection used by the client.
///
/// Use a `max_attempts` value of `0` to retry forever.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconnectPolicy {
  /// Wait a constant amount of time between reconnect attempts, in ms.
  Constant {
    attempts:     u32,
    max_attempts: u32,
    delay:        u32,
    jitter:       u32,
  },
  /// Backoff reconnection attempts linearly, adding `delay` each time.
  Linear {
    attempts:     u32,
    max_attempts: u32,
    max_delay:    u32,
    delay:        u32,
    jitter:       u32,
  },
  /// Backoff reconnection attempts exponentially, multiplying the last delay by `base` each time.
  Exponential {
    attempts:     u32,
    max_attempts: u32,
    min_delay:    u32,
    max_delay:    u32,
    base:         u32,
    jitter:       u32,
  },
}

impl Default for ReconnectPolicy {
  fn default() -> Self {
    ReconnectPolicy::Constant {
      attempts:     0,
      max_attempts: 0,
      delay:        1000,
      jitter:       DEFAULT_JITTER_MS,
    }
  }
}

impl ReconnectPolicy {
  /// Create a new reconnect policy with a constant backoff.
  pub fn new_constant(max_attempts: u32, delay: u32) -> ReconnectPolicy {
    ReconnectPolicy::Constant {
      max_attempts,
      delay,
      attempts: 0,
      jitter: DEFAULT_JITTER_MS,
    }
  }

  /// Create a new reconnect policy with a linear backoff.
  pub fn new_linear(max_attempts: u32, max_delay: u32, delay: u32) -> ReconnectPolicy {
    ReconnectPolicy::Linear {
      max_attempts,
      max_delay,
      delay,
      attempts: 0,
      jitter: DEFAULT_JITTER_MS,
    }
  }

  /// Create a new reconnect policy with an exponential backoff.
  pub fn new_exponential(max_attempts: u32, min_delay: u32, max_delay: u32, base: u32) -> ReconnectPolicy {
    ReconnectPolicy::Exponential {
      max_delay,
      max_attempts,
      min_delay,
      base,
      attempts: 0,
      jitter: DEFAULT_JITTER_MS,
    }
  }

  /// Set the amount of jitter to add to each reconnect delay.
  ///
  /// Default: 50 ms
  pub fn set_jitter(&mut self, jitter_ms: u32) {
    match self {
      ReconnectPolicy::Constant { ref mut jitter, .. } => {
        *jitter = jitter_ms;
      },
      ReconnectPolicy::Linear { ref mut jitter, .. } => {
        *jitter = jitter_ms;
      },
      ReconnectPolicy::Exponential { ref mut jitter, .. } => {
        *jitter = jitter_ms;
      },
    }
  }

  /// Reset the number of reconnection attempts.
  pub(crate) fn reset_attempts(&mut self) {
    match *self {
      ReconnectPolicy::Constant { ref mut attempts, .. } => {
        *attempts = 0;
      },
      ReconnectPolicy::Linear { ref mut attempts, .. } => {
        *attempts = 0;
      },
      ReconnectPolicy::Exponential { ref mut attempts, .. } => {
        *attempts = 0;
      },
    }
  }

  /// Read the number of reconnection attempts.
  pub fn attempts(&self) -> u32 {
    match self {
      ReconnectPolicy::Constant { ref attempts, .. } => *attempts,
      ReconnectPolicy::Linear { ref attempts, .. } => *attempts,
      ReconnectPolicy::Exponential { ref attempts, .. } => *attempts,
    }
  }

  /// Read the max number of reconnection attempts.
  pub fn max_attempts(&self) -> u32 {
    match self {
      ReconnectPolicy::Constant { ref max_attempts, .. } => *max_attempts,
      ReconnectPolicy::Linear { ref max_attempts, .. } => *max_attempts,
      ReconnectPolicy::Exponential { ref max_attempts, .. } => *max_attempts,
    }
  }

  /// Whether the client should initiate a reconnect.
  pub(crate) fn should_reconnect(&self) -> bool {
    match *self {
      ReconnectPolicy::Constant {
        ref attempts,
        ref max_attempts,
        ..
      } => *max_attempts == 0 || *attempts < *max_attempts,
      ReconnectPolicy::Linear {
        ref attempts,
        ref max_attempts,
        ..
      } => *max_attempts == 0 || *attempts < *max_attempts,
      ReconnectPolicy::Exponential {
        ref attempts,
        ref max_attempts,
        ..
      } => *max_attempts == 0 || *attempts < *max_attempts,
    }
  }

  /// Calculate the next delay, incrementing `attempts` in the process.
  pub fn next_delay(&mut self) -> Option<u64> {
    match *self {
      ReconnectPolicy::Constant {
        ref mut attempts,
        delay,
        max_attempts,
        jitter,
      } => {
        *attempts = utils::incr_with_max(*attempts, max_attempts)?;

        Some(utils::add_jitter(delay as u64, jitter))
      },
      ReconnectPolicy::Linear {
        ref mut attempts,
        max_delay,
        max_attempts,
        delay,
        jitter,
      } => {
        *attempts = utils::incr_with_max(*attempts, max_attempts)?;
        let delay = (delay as u64).saturating_mul(*attempts as u64);

        Some(cmp::min(max_delay as u64, utils::add_jitter(delay, jitter)))
      },
      ReconnectPolicy::Exponential {
        ref mut attempts,
        min_delay,
        max_delay,
        max_attempts,
        base,
        jitter,
      } => {
        *attempts = utils::incr_with_max(*attempts, max_attempts)?;
        let delay = (base as u64)
          .saturating_pow(*attempts - 1)
          .saturating_mul(min_delay as u64);

        Some(cmp::min(max_delay as u64, utils::add_jitter(delay, jitter)))
      },
    }
  }
}

/// Describes how the client should respond when a command is sent while the client is in a blocked state from a
/// blocking command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Blocking {
  /// Wait to send the command until the blocked command finishes. (Default)
  Block,
  /// Return an error to the caller.
  Error,
  /// Interrupt the blocked command by automatically sending `CLIENT UNBLOCK` for the blocked connection.
  Interrupt,
}

impl Default for Blocking {
  fn default() -> Self {
    Blocking::Block
  }
}

/// TCP configuration options.
#[derive(Clone, Debug, Default)]
pub struct TcpConfig {
  /// Set the [TCP_NODELAY](https://docs.rs/tokio/latest/tokio/net/struct.TcpStream.html#method.set_nodelay) value.
  pub nodelay:      Option<bool>,
  /// Set the [SO_LINGER](https://docs.rs/tokio/latest/tokio/net/struct.TcpStream.html#method.set_linger) value.
  pub linger:       Option<Duration>,
  /// Set the [IP_TTL](https://docs.rs/tokio/latest/tokio/net/struct.TcpStream.html#method.set_ttl) value.
  pub ttl:          Option<u32>,
  /// Set the [TCP keepalive values](https://docs.rs/socket2/latest/socket2/struct.Socket.html#method.set_tcp_keepalive).
  pub keepalive:    Option<TcpKeepalive>,
  /// Set the [TCP_USER_TIMEOUT](https://docs.rs/socket2/latest/x86_64-unknown-linux-gnu/socket2/struct.Socket.html#method.set_tcp_user_timeout) value.
  #[cfg(all(
    feature = "tcp-user-timeouts",
    not(feature = "glommio"),
    any(target_os = "android", target_os = "fuchsia", target_os = "linux")
  ))]
  #[cfg_attr(
    docsrs,
    doc(cfg(all(
      feature = "tcp-user-timeouts",
      not(feature = "glommio"),
      any(target_os = "android", target_os = "fuchsia", target_os = "linux")
    )))
  )]
  pub user_timeout: Option<Duration>,
}

impl PartialEq for TcpConfig {
  fn eq(&self, other: &Self) -> bool {
    self.nodelay == other.nodelay && self.linger == other.linger && self.ttl == other.ttl
  }
}

impl Eq for TcpConfig {}

/// Configuration options used to detect potentially unresponsive connections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresponsiveConfig {
  /// If provided, the amount of time a frame can wait without a response before the associated connection is
  /// considered unresponsive.
  ///
  /// If a connection is considered unresponsive it will be forcefully closed and the client will reconnect based on
  /// the [ReconnectPolicy](crate::types::config::ReconnectPolicy). This heuristic can be useful in environments
  /// where connections may close or change in subtle or unexpected ways.
  ///
  /// Unlike the [timeout](crate::types::config::Options) and
  /// [default_command_timeout](crate::types::config::PerformanceConfig) interfaces, any in-flight commands waiting
  /// on a response when the connection is closed this way will be retried based on the associated
  /// [ReconnectPolicy](crate::types::config::ReconnectPolicy) and [Options](crate::types::config::Options).
  ///
  /// Default: `None`
  pub max_timeout: Option<Duration>,
  /// The frequency at which the client checks for unresponsive connections.
  ///
  /// This value should usually be less than half of `max_timeout` and always more than 1 ms.
  ///
  /// Default: 2 sec
  pub interval:    Duration,
}

impl Default for UnresponsiveConfig {
  fn default() -> Self {
    UnresponsiveConfig {
      max_timeout: None,
      interval:    Duration::from_secs(2),
    }
  }
}

/// A policy that determines how clustered clients initially connect to and discover other cluster nodes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterDiscoveryPolicy {
  /// Always use the endpoint(s) provided in the client's [ServerConfig](ServerConfig).
  ///
  /// This is generally recommended with managed services, Kubernetes, or other systems that provide client routing
  /// or cluster discovery interfaces.
  ///
  /// Default.
  ConfigEndpoint,
  /// Try connecting to nodes specified in both the client's [ServerConfig](ServerConfig) and the most recently
  /// cached routing table.
  UseCache,
}

impl Default for ClusterDiscoveryPolicy {
  fn default() -> Self {
    ClusterDiscoveryPolicy::ConfigEndpoint
  }
}

/// Configuration options related to the creation or management of TCP connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionConfig {
  /// The timeout to apply when attempting to create a new TCP connection.
  ///
  /// This also includes the TLS handshake if using any of the TLS features.
  ///
  /// Default: 10 sec
  pub connection_timeout:           Duration,
  /// The timeout to apply when sending internal commands such as `AUTH`, `SELECT`, `CLUSTER SLOTS`, `READONLY`, etc.
  ///
  /// Default: 10 sec
  pub internal_command_timeout:     Duration,
  /// The amount of time to wait after a `MOVED` error is received before the client will update the cached cluster
  /// state.
  ///
  /// Default: `0`
  pub cluster_cache_update_delay:   Duration,
  /// The maximum number of times the client will attempt to send a command.
  ///
  /// This value be incremented whenever the connection closes while the command is in-flight.
  ///
  /// Default: `3`
  pub max_command_attempts:         u32,
  /// The maximum number of times the client will attempt to follow a `MOVED` or `ASK` redirection per command.
  ///
  /// Default: `5`
  pub max_redirections:             u32,
  /// Unresponsive connection configuration options.
  pub unresponsive:                 UnresponsiveConfig,
  /// An unexpected `NOAUTH` error is treated the same as a general connection failure, causing the client to
  /// reconnect based on the [ReconnectPolicy](crate::types::config::ReconnectPolicy). This is [recommended](https://github.com/StackExchange/StackExchange.Redis/issues/1273#issuecomment-651823824) if callers are using ElastiCache.
  ///
  /// Default: `false`
  pub reconnect_on_auth_error:      bool,
  /// Automatically send `CLIENT SETNAME` on each connection associated with a client instance.
  ///
  /// Default: `false`
  pub auto_client_setname:          bool,
  /// Limit the size of the internal in-memory command queue.
  ///
  /// Commands that exceed this limit will receive a `ErrorKind::Backpressure` error. Setting this value to
  /// anything > 0 will indicate that the client should use a bounded MPSC channel to communicate with the routing
  /// task.
  ///
  /// See [command_queue_len](crate::interfaces::MetricsInterface::command_queue_len) for more information.
  ///
  /// Default: `0` (unlimited)
  pub max_command_buffer_len:       usize,
  /// Disable the `CLUSTER INFO` health check when initializing cluster connections.
  ///
  /// Default: `false`
  pub disable_cluster_health_check: bool,
  /// Configuration options for replica nodes.
  ///
  /// Default: `None`
  #[cfg(feature = "replicas")]
  #[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
  pub replica:                      ReplicaConfig,
  /// TCP connection options.
  pub tcp:                          TcpConfig,
  /// Errors that should trigger reconnection logic.
  #[cfg(feature = "custom-reconnect-errors")]
  #[cfg_attr(docsrs, doc(cfg(feature = "custom-reconnect-errors")))]
  pub reconnect_errors:             Vec<ReconnectError>,

  /// The task queue onto which routing tasks will be spawned.
  ///
  /// May cause a panic if [spawn_local_into](glommio::spawn_local_into) fails.
  #[cfg(feature = "glommio")]
  #[cfg_attr(docsrs, doc(cfg(feature = "glommio")))]
  pub router_task_queue: Option<glommio::TaskQueueHandle>,

  /// The task queue onto which connection reader tasks will be spawned.
  ///
  /// May cause a panic if [spawn_local_into](glommio::spawn_local_into) fails.
  #[cfg(feature = "glommio")]
  #[cfg_attr(docsrs, doc(cfg(feature = "glommio")))]
  pub connection_task_queue: Option<glommio::TaskQueueHandle>,
}

impl Default for ConnectionConfig {
  fn default() -> Self {
    #[allow(deprecated)]
    ConnectionConfig {
      connection_timeout: Duration::from_millis(10_000),
      internal_command_timeout: Duration::from_millis(10_000),
      max_redirections: 5,
      max_command_attempts: 3,
      max_command_buffer_len: 0,
      auto_client_setname: false,
      cluster_cache_update_delay: Duration::from_millis(0),
      reconnect_on_auth_error: false,
      disable_cluster_health_check: false,
      tcp: TcpConfig::default(),
      unresponsive: UnresponsiveConfig::default(),
      #[cfg(feature = "replicas")]
      replica: ReplicaConfig::default(),
      #[cfg(feature = "custom-reconnect-errors")]
      reconnect_errors: vec![
        ReconnectError::ClusterDown,
        ReconnectError::Loading,
        ReconnectError::ReadOnly,
      ],
      #[cfg(feature = "glommio")]
      router_task_queue: None,
      #[cfg(feature = "glommio")]
      connection_task_queue: None,
    }
  }
}

/// Configuration options that can affect the performance of the client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PerformanceConfig {
  /// An optional timeout to apply to all commands.
  ///
  /// If `0` this will disable any timeout being applied to commands. Callers can also set timeouts on individual
  /// commands via the [with_options](crate::interfaces::ClientLike::with_options) interface.
  ///
  /// Default: `0`
  pub default_command_timeout:    Duration,
  /// The maximum number of frames that will be fed to a socket before flushing.
  ///
  /// Note: in some circumstances the client with always flush the socket (`QUIT`, `EXEC`, etc).
  ///
  /// Default: 200
  pub max_feed_count:             u64,
  /// The default capacity used when creating [broadcast channels](https://docs.rs/tokio/latest/tokio/sync/broadcast/fn.channel.html) in the [EventInterface](crate::interfaces::EventInterface).
  ///
  /// Default: 32
  pub broadcast_channel_capacity: usize,
  /// The minimum size, in bytes, of frames that should be encoded or decoded with a blocking task.
  ///
  /// See [block_in_place](https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html) for more information.
  ///
  /// Default: 50_000_000
  #[cfg(feature = "blocking-encoding")]
  #[cfg_attr(docsrs, doc(cfg(feature = "blocking-encoding")))]
  pub blocking_encode_threshold:  usize,
}

impl Default for PerformanceConfig {
  fn default() -> Self {
    PerformanceConfig {
      default_command_timeout:                                         Duration::from_millis(0),
      max_feed_count:                                                  200,
      broadcast_channel_capacity:                                      32,
      #[cfg(feature = "blocking-encoding")]
      blocking_encode_threshold:                                       50_000_000,
    }
  }
}

/// A trait that can be used to override the credentials used in each `AUTH` or `HELLO` command.
#[async_trait]
#[cfg(all(feature = "credential-provider", not(feature = "glommio")))]
#[cfg_attr(docsrs, doc(cfg(feature = "credential-provider")))]
pub trait CredentialProvider: Debug + Send + Sync + 'static {
  /// Read the username and password that should be used in the next `AUTH` or `HELLO` command.
  async fn fetch(&self, server: Option<&Server>) -> Result<(Option<String>, Option<String>), Error>;

  /// Configure the client to call [fetch](Self::fetch) and send `AUTH` or `HELLO` on some interval.
  fn refresh_interval(&self) -> Option<Duration> {
    None
  }
}

/// A trait that can be used to override the credentials used in each `AUTH` or `HELLO` command.
#[async_trait(?Send)]
#[cfg(all(feature = "credential-provider", feature = "glommio"))]
#[cfg_attr(docsrs, doc(cfg(feature = "credential-provider")))]
pub trait CredentialProvider: Debug + 'static {
  /// Read the username and password that should be used in the next `AUTH` or `HELLO` command.
  async fn fetch(&self, server: Option<&Server>) -> Result<(Option<String>, Option<String>), Error>;

  /// Configure the client to call [fetch](Self::fetch) and send `AUTH` or `HELLO` on some interval.
  fn refresh_interval(&self) -> Option<Duration> {
    None
  }
}

/// Configuration options for a `Client`.
#[derive(Clone, Debug)]
pub struct Config {
  /// Whether the client should return an error if it cannot connect to the server the first time when being
  /// initialized. If `false` the client will run the reconnect logic if it cannot connect to the server the first
  /// time, but if `true` the client will return initial connection errors to the caller immediately.
  ///
  /// Normally the reconnection logic only applies to connections that close unexpectedly, but this flag can apply
  /// the same logic to the first connection as it is being created.
  ///
  /// Callers should use caution setting this to `false` since it can make debugging configuration issues more
  /// difficult.
  ///
  /// Default: `true`
  pub fail_fast: bool,
  /// The default behavior of the client when a command is sent while the connection is blocked on a blocking
  /// command.
  ///
  /// Setting this to anything other than `Blocking::Block` incurs a small performance penalty.
  ///
  /// Default: `Blocking::Block`
  pub blocking:  Blocking,
  /// An optional ACL username for the client to use when authenticating. If ACL rules are not configured this should
  /// be `None`.
  ///
  /// Default: `None`
  pub username:  Option<String>,
  /// An optional password for the client to use when authenticating.
  ///
  /// Default: `None`
  pub password:  Option<String>,

  /// Connection configuration for the server(s).
  ///
  /// Default: `Centralized(localhost, 6379)`
  pub server:              ServerConfig,
  /// The protocol version to use when communicating with the server(s).
  ///
  /// If RESP3 is specified the client will automatically use `HELLO` when authenticating. **This requires version
  /// 6.0.0 or above.** If the `HELLO` command fails this will prevent the client from connecting. Callers should set
  /// this to RESP2 and use `HELLO` manually to fall back to RESP2 if needed.
  ///
  /// Note: upgrading an existing codebase from RESP2 to RESP3 may require changing certain type signatures. RESP3
  /// has a slightly different type system than RESP2.
  ///
  /// Default: `RESP2`
  pub version:             RespVersion,
  /// An optional database number that the client will automatically `SELECT` after connecting or reconnecting.
  ///
  /// It is recommended that callers use this field instead of putting a `select()` call inside the `on_reconnect`
  /// block, if possible. Commands that were in-flight when the connection closed will retry before anything inside
  /// the `on_reconnect` block.
  ///
  /// Default: `None`
  pub database:            Option<u8>,
  /// TLS configuration options.
  ///
  /// Default: `None`
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  #[cfg_attr(
    docsrs,
    doc(cfg(any(
      feature = "enable-native-tls",
      feature = "enable-rustls",
      feature = "enable-rustls-ring"
    )))
  )]
  pub tls:                 Option<TlsConfig>,
  /// Tracing configuration options.
  #[cfg(feature = "partial-tracing")]
  #[cfg_attr(docsrs, doc(cfg(feature = "partial-tracing")))]
  pub tracing:             TracingConfig,
  /// An optional [mocking layer](crate::mocks) to intercept and process commands.
  ///
  /// Default: `None`
  #[cfg(feature = "mocks")]
  #[cfg_attr(docsrs, doc(cfg(feature = "mocks")))]
  pub mocks:               Option<Arc<dyn Mocks>>,
  /// An optional credential provider callback interface.
  ///
  /// Default: `None`
  ///
  /// When used with the `sentinel-auth` feature this interface will take precedence over all `username` and
  /// `password` fields for both sentinel nodes and servers.
  #[cfg(feature = "credential-provider")]
  #[cfg_attr(docsrs, doc(cfg(feature = "credential-provider")))]
  pub credential_provider: Option<Arc<dyn CredentialProvider>>,
}

impl PartialEq for Config {
  fn eq(&self, other: &Self) -> bool {
    self.server == other.server
      && self.database == other.database
      && self.fail_fast == other.fail_fast
      && self.version == other.version
      && self.username == other.username
      && self.password == other.password
      && self.blocking == other.blocking
  }
}

impl Eq for Config {}

impl Default for Config {
  fn default() -> Self {
    Config {
      fail_fast: true,
      blocking: Blocking::default(),
      username: None,
      password: None,
      server: ServerConfig::default(),
      version: RespVersion::RESP2,
      database: None,
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      tls: None,
      #[cfg(feature = "partial-tracing")]
      tracing: TracingConfig::default(),
      #[cfg(feature = "mocks")]
      mocks: None,
      #[cfg(feature = "credential-provider")]
      credential_provider: None,
    }
  }
}

#[cfg_attr(docsrs, allow(rustdoc::broken_intra_doc_links))]
impl Config {
  /// Whether the client uses TLS.
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  pub fn uses_tls(&self) -> bool {
    self.tls.is_some()
  }

  /// Whether the client uses TLS.
  #[cfg(not(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  )))]
  pub fn uses_tls(&self) -> bool {
    false
  }

  /// Whether the client uses a `native-tls` connector.
  #[cfg(feature = "enable-native-tls")]
  pub fn uses_native_tls(&self) -> bool {
    self
      .tls
      .as_ref()
      .map(|config| matches!(config.connector, TlsConnector::Native(_)))
      .unwrap_or(false)
  }

  /// Whether the client uses a `native-tls` connector.
  #[cfg(not(feature = "enable-native-tls"))]
  pub fn uses_native_tls(&self) -> bool {
    false
  }

  /// Whether the client uses a `rustls` connector.
  #[cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))]
  pub fn uses_rustls(&self) -> bool {
    self
      .tls
      .as_ref()
      .map(|config| matches!(config.connector, TlsConnector::Rustls(_)))
      .unwrap_or(false)
  }

  /// Whether the client uses a `rustls` connector.
  #[cfg(not(any(feature = "enable-rustls", feature = "enable-rustls-ring")))]
  pub fn uses_rustls(&self) -> bool {
    false
  }

  /// Parse a URL string into a `Config`.
  ///
  /// # URL Syntax
  ///
  /// **Centralized**
  ///
  /// ```text
  /// redis|rediss :// [[username:]password@] host [:port][/database]
  /// ```
  ///
  /// **Clustered**
  ///
  /// ```text
  /// redis|rediss[-cluster] :// [[username:]password@] host [:port][?[node=host1:port1][&node=host2:port2][&node=hostN:portN]]
  /// ```
  ///
  /// **Sentinel**
  ///
  /// ```text
  /// redis|rediss[-sentinel] :// [[username1:]password1@] host [:port][/database][?[node=host1:port1][&node=host2:port2][&node=hostN:portN]
  ///                             [&sentinelServiceName=myservice][&sentinelUsername=username2][&sentinelPassword=password2]]
  /// ```
  ///
  /// **Unix Socket**
  ///
  /// ```text
  /// redis+unix:// [[username:]password@] /path/to/redis.sock
  /// ```
  ///
  /// # Schemes
  ///
  /// This function will use the URL scheme to determine which server type the caller is using. Valid schemes include:
  ///
  /// * `redis|valkey` - TCP connected to a centralized server.
  /// * `rediss|valkeys` - TLS connected to a centralized server.
  /// * `redis-cluster|valkey-cluster` - TCP connected to a cluster.
  /// * `rediss-cluster|valkeys-cluster` - TLS connected to a cluster.
  /// * `redis-sentinel|valkey-sentinel` - TCP connected to a centralized server behind a sentinel layer.
  /// * `rediss-sentinel|valkeys-sentinel` - TLS connected to a centralized server behind a sentinel layer.
  /// * `redis+unix|valkey+unix` - Unix domain socket followed by a path.
  ///
  /// **The `rediss|valkeys` scheme prefix requires one of the TLS feature flags.**
  ///
  /// # Query Parameters
  ///
  /// In some cases it's necessary to specify multiple node hostname/port tuples (with a cluster or sentinel layer for
  /// example). The following query parameters may also be used in their respective contexts:
  ///
  /// * `node` - Specify another node in the topology. In a cluster this would refer to any other known cluster node.
  ///   In the context of a sentinel layer this refers to a known **sentinel** node. Multiple `node` parameters may be
  ///   used in a URL.
  /// * `sentinelServiceName` - Specify the name of the sentinel service. This is required when using the
  ///   `redis-sentinel` scheme.
  /// * `sentinelUsername` - Specify the username to use when connecting to a **sentinel** node. This requires the
  ///   `sentinel-auth` feature and allows the caller to use different credentials for sentinel nodes vs the actual
  ///   server. The `username` part of the URL immediately following the scheme will refer to the username used when
  ///   connecting to the backing server.
  /// * `sentinelPassword` - Specify the password to use when connecting to a **sentinel** node. This requires the
  ///   `sentinel-auth` feature and allows the caller to use different credentials for sentinel nodes vs the actual
  ///   server. The `password` part of the URL immediately following the scheme will refer to the password used when
  ///   connecting to the backing server.
  ///
  /// See the [from_url_centralized](Self::from_url_centralized), [from_url_clustered](Self::from_url_clustered),
  /// [from_url_sentinel](Self::from_url_sentinel), and [from_url_unix](Self::from_url_unix) for more information. Or
  /// see the [Config](Self) unit tests for examples.
  pub fn from_url(url: &str) -> Result<Config, Error> {
    let parsed_url = Url::parse(url)?;
    if utils::url_is_clustered(&parsed_url) {
      Config::from_url_clustered(url)
    } else if utils::url_is_sentinel(&parsed_url) {
      Config::from_url_sentinel(url)
    } else if utils::url_is_unix_socket(&parsed_url) {
      #[cfg(feature = "unix-sockets")]
      return Config::from_url_unix(url);
      #[allow(unreachable_code)]
      Err(Error::new(ErrorKind::Config, "Missing unix-socket feature."))
    } else {
      Config::from_url_centralized(url)
    }
  }

  /// Create a centralized `Config` struct from a URL.
  ///
  /// ```text
  /// redis://username:password@foo.com:6379/1
  /// rediss://username:password@foo.com:6379/1
  /// redis://foo.com:6379/1
  /// redis://foo.com
  /// // ... etc
  /// ```
  ///
  /// This function is very similar to [from_url](Self::from_url), but it adds a layer of validation for configuration
  /// parameters that are only relevant to a centralized server.
  ///
  /// For example:
  ///
  /// * A database can be defined in the `path` section.
  /// * The `port` field is optional in this context. If it is not specified then `6379` will be used.
  /// * Any `node` or sentinel query parameters will be ignored.
  pub fn from_url_centralized(url: &str) -> Result<Config, Error> {
    let (url, host, port, _tls) = utils::parse_url(url, Some(6379))?;
    let server = ServerConfig::new_centralized(host, port);
    let database = utils::parse_url_db(&url)?;
    let (username, password) = utils::parse_url_credentials(&url)?;

    Ok(Config {
      server,
      username,
      password,
      database,
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      tls: utils::tls_config_from_url(_tls)?,
      ..Config::default()
    })
  }

  /// Create a clustered `Config` struct from a URL.
  ///
  /// ```text
  /// redis-cluster://username:password@foo.com:30001?node=bar.com:30002&node=baz.com:30003
  /// rediss-cluster://username:password@foo.com:30001?node=bar.com:30002&node=baz.com:30003
  /// rediss://foo.com:30001?node=bar.com:30002&node=baz.com:30003
  /// redis://foo.com:30001
  /// // ... etc
  /// ```
  ///
  /// This function is very similar to [from_url](Self::from_url), but it adds a layer of validation for configuration
  /// parameters that are only relevant to a clustered deployment.
  ///
  /// For example:
  ///
  /// * The `-cluster` suffix in the scheme is optional when using this function directly.
  /// * Any database defined in the `path` section will be ignored.
  /// * The `port` field is required in this context alongside any hostname.
  /// * Any `node` query parameters will be used to find other known cluster nodes.
  /// * Any sentinel query parameters will be ignored.
  pub fn from_url_clustered(url: &str) -> Result<Config, Error> {
    let (url, host, port, _tls) = utils::parse_url(url, Some(6379))?;
    let mut cluster_nodes = utils::parse_url_other_nodes(&url)?;
    cluster_nodes.push(Server::new(host, port));
    let server = ServerConfig::Clustered {
      hosts:  cluster_nodes,
      policy: ClusterDiscoveryPolicy::default(),
    };
    let (username, password) = utils::parse_url_credentials(&url)?;

    Ok(Config {
      server,
      username,
      password,
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      tls: utils::tls_config_from_url(_tls)?,
      ..Config::default()
    })
  }

  /// Create a sentinel `Config` struct from a URL.
  ///
  /// ```text
  /// redis-sentinel://username:password@foo.com:6379/1?sentinelServiceName=fakename&node=foo.com:30001&node=bar.com:30002
  /// rediss-sentinel://username:password@foo.com:6379/0?sentinelServiceName=fakename&node=foo.com:30001&node=bar.com:30002
  /// redis://foo.com:6379?sentinelServiceName=fakename
  /// rediss://foo.com:6379/1?sentinelServiceName=fakename
  /// // ... etc
  /// ```
  ///
  /// This function is very similar to [from_url](Self::from_url), but it adds a layer of validation for configuration
  /// parameters that are only relevant to a sentinel deployment.
  ///
  /// For example:
  ///
  /// * The `-sentinel` suffix in the scheme is optional when using this function directly.
  /// * A database can be defined in the `path` section.
  /// * The `port` field is optional following the first hostname (`26379` will be used if undefined), but required
  ///   within any `node` query parameters.
  /// * Any `node` query parameters will be used to find other known sentinel nodes.
  /// * The `sentinelServiceName` query parameter is required.
  /// * Depending on the cargo features used other sentinel query parameters may be used.
  ///
  /// This particular function is more complex than the others when the `sentinel-auth` feature is used. For example,
  /// to declare a config that uses different credentials for the sentinel nodes vs the backing servers:
  ///
  /// ```text
  /// redis-sentinel://username1:password1@foo.com:26379/1?sentinelServiceName=fakename&sentinelUsername=username2&sentinelPassword=password2&node=bar.com:26379&node=baz.com:26380
  /// ```
  ///
  /// The above example will use `("username1", "password1")` when authenticating to the backing servers, and
  /// `("username2", "password2")` when initially connecting to the sentinel nodes. Additionally, all 3 addresses
  /// (`foo.com:26379`, `bar.com:26379`, `baz.com:26380`) specify known **sentinel** nodes.
  pub fn from_url_sentinel(url: &str) -> Result<Config, Error> {
    let (url, host, port, _tls) = utils::parse_url(url, Some(26379))?;
    let mut other_nodes = utils::parse_url_other_nodes(&url)?;
    other_nodes.push(Server::new(host, port));
    let service_name = utils::parse_url_sentinel_service_name(&url)?;
    let (username, password) = utils::parse_url_credentials(&url)?;
    let database = utils::parse_url_db(&url)?;
    let server = ServerConfig::Sentinel {
      hosts: other_nodes,
      service_name,
      #[cfg(feature = "sentinel-auth")]
      username: utils::parse_url_sentinel_username(&url),
      #[cfg(feature = "sentinel-auth")]
      password: utils::parse_url_sentinel_password(&url),
    };

    Ok(Config {
      server,
      username,
      password,
      database,
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      tls: utils::tls_config_from_url(_tls)?,
      ..Config::default()
    })
  }

  /// Create a `Config` from a URL that connects via a Unix domain socket.
  ///
  /// ```text
  /// redis+unix:///path/to/redis.sock
  /// redis+unix://username:password@nonemptyhost/path/to/redis.sock
  /// ```
  ///
  /// **Important**
  ///
  /// * In the other URL parsing functions the path section indicates the database that the client should `SELECT`
  ///   after connecting. However, Unix sockets are also specified by a path rather than a hostname:port, which
  ///   creates some ambiguity in this case. Callers should manually set the database field on the returned `Config`
  ///   if needed.
  /// * If credentials are provided the caller must also specify a hostname in order to pass to the [URL
  ///   validation](Url::parse) process. This function will ignore the value, but some non-empty string must be
  ///   provided.
  #[cfg(feature = "unix-sockets")]
  #[cfg_attr(docsrs, doc(cfg(feature = "unix-sockets")))]
  pub fn from_url_unix(url: &str) -> Result<Config, Error> {
    let (url, path) = utils::parse_unix_url(url)?;
    let (username, password) = utils::parse_url_credentials(&url)?;

    Ok(Config {
      server: ServerConfig::Unix { path },
      username,
      password,
      ..Default::default()
    })
  }
}

/// Connection configuration for the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerConfig {
  Centralized {
    /// The `Server` identifier.
    server: Server,
  },
  Clustered {
    /// The known cluster node `Server` identifiers.
    ///
    /// Only one node in the cluster needs to be provided here, the rest will be discovered via the `CLUSTER SLOTS`
    /// command.
    hosts:  Vec<Server>,
    /// The cluster discovery policy to use when connecting or following redirections.
    policy: ClusterDiscoveryPolicy,
  },
  #[cfg(feature = "unix-sockets")]
  #[cfg_attr(docsrs, doc(cfg(feature = "unix-sockets")))]
  Unix {
    /// The path to the Unix socket.
    ///
    /// Any associated [Server](crate::types::config::Server) identifiers will use this value as the `host`.
    path: PathBuf,
  },
  Sentinel {
    /// An array of `Server` identifiers for each known sentinel instance.
    hosts:        Vec<Server>,
    /// The service name for primary/main instances.
    service_name: String,

    /// An optional ACL username for the client to use when authenticating.
    #[cfg(feature = "sentinel-auth")]
    #[cfg_attr(docsrs, doc(cfg(feature = "sentinel-auth")))]
    username: Option<String>,
    /// An optional password for the client to use when authenticating.
    #[cfg(feature = "sentinel-auth")]
    #[cfg_attr(docsrs, doc(cfg(feature = "sentinel-auth")))]
    password: Option<String>,
  },
}

impl Default for ServerConfig {
  fn default() -> Self {
    ServerConfig::default_centralized()
  }
}

impl ServerConfig {
  /// Create a new centralized config with the provided host and port.
  pub fn new_centralized<S>(host: S, port: u16) -> ServerConfig
  where
    S: Into<String>,
  {
    ServerConfig::Centralized {
      server: Server::new(host.into(), port),
    }
  }

  /// Create a new clustered config with the provided set of hosts and ports.
  ///
  /// Only one valid host in the cluster needs to be provided here. The client will use `CLUSTER NODES` to discover
  /// the other nodes.
  pub fn new_clustered<S>(mut hosts: Vec<(S, u16)>) -> ServerConfig
  where
    S: Into<String>,
  {
    ServerConfig::Clustered {
      hosts:  hosts.drain(..).map(|(s, p)| Server::new(s.into(), p)).collect(),
      policy: ClusterDiscoveryPolicy::default(),
    }
  }

  /// Create a new sentinel config with the provided set of hosts and the name of the service.
  ///
  /// This library will connect using the details from the [Redis documentation](https://redis.io/topics/sentinel-clients).
  pub fn new_sentinel<H, N>(hosts: Vec<(H, u16)>, service_name: N) -> ServerConfig
  where
    H: Into<String>,
    N: Into<String>,
  {
    ServerConfig::Sentinel {
      hosts:                                      hosts.into_iter().map(|(h, p)| Server::new(h.into(), p)).collect(),
      service_name:                               service_name.into(),
      #[cfg(feature = "sentinel-auth")]
      username:                                   None,
      #[cfg(feature = "sentinel-auth")]
      password:                                   None,
    }
  }

  /// Create a new server config for a connected Unix socket.
  #[cfg(feature = "unix-sockets")]
  #[cfg_attr(docsrs, doc(cfg(feature = "unix-sockets")))]
  pub fn new_unix_socket<P>(path: P) -> ServerConfig
  where
    P: Into<PathBuf>,
  {
    ServerConfig::Unix { path: path.into() }
  }

  /// Create a centralized config with default settings for a local deployment.
  pub fn default_centralized() -> ServerConfig {
    ServerConfig::Centralized {
      server: Server::new("127.0.0.1", 6379),
    }
  }

  /// Create a clustered config with the same defaults as specified in the `create-cluster` script provided by Redis
  /// or Valkey.
  pub fn default_clustered() -> ServerConfig {
    ServerConfig::Clustered {
      hosts:  vec![
        Server::new("127.0.0.1", 30001),
        Server::new("127.0.0.1", 30002),
        Server::new("127.0.0.1", 30003),
      ],
      policy: ClusterDiscoveryPolicy::default(),
    }
  }

  /// Whether the config uses a clustered deployment.
  pub fn is_clustered(&self) -> bool {
    matches!(*self, ServerConfig::Clustered { .. })
  }

  /// Whether the config is for a centralized server behind a sentinel node(s).
  pub fn is_sentinel(&self) -> bool {
    matches!(*self, ServerConfig::Sentinel { .. })
  }

  /// Whether the config is for a centralized server.
  pub fn is_centralized(&self) -> bool {
    matches!(*self, ServerConfig::Centralized { .. })
  }

  /// Whether the config uses a Unix socket.
  pub fn is_unix_socket(&self) -> bool {
    match *self {
      #[cfg(feature = "unix-sockets")]
      ServerConfig::Unix { .. } => true,
      _ => false,
    }
  }

  /// Read the server hosts or sentinel hosts if using the sentinel interface.
  pub fn hosts(&self) -> Vec<Server> {
    match *self {
      ServerConfig::Centralized { ref server } => vec![server.clone()],
      ServerConfig::Clustered { ref hosts, .. } => hosts.to_vec(),
      ServerConfig::Sentinel { ref hosts, .. } => hosts.to_vec(),
      #[cfg(feature = "unix-sockets")]
      ServerConfig::Unix { ref path } => vec![Server::new(utils::path_to_string(path), 0)],
    }
  }

  /// Set the [ClusterDiscoveryPolicy], if possible.
  pub fn set_cluster_discovery_policy(&mut self, new_policy: ClusterDiscoveryPolicy) -> Result<(), Error> {
    if let ServerConfig::Clustered { ref mut policy, .. } = self {
      *policy = new_policy;
      Ok(())
    } else {
      Err(Error::new(ErrorKind::Config, "Expected clustered config."))
    }
  }
}

/// Configuration options for tracing.
#[cfg(feature = "partial-tracing")]
#[cfg_attr(docsrs, doc(cfg(feature = "partial-tracing")))]
#[derive(Clone, Debug)]
pub struct TracingConfig {
  /// Whether to enable tracing for this client.
  ///
  /// Default: `false`
  pub enabled: bool,

  /// Set the `tracing::Level` of spans under `partial-tracing` feature.
  ///
  /// Default: `INFO`
  pub default_tracing_level: tracing::Level,

  /// Set the `tracing::Level` of spans under `full-tracing` feature.
  ///
  /// Default: `DEBUG`
  #[cfg(feature = "full-tracing")]
  #[cfg_attr(docsrs, doc(cfg(feature = "full-tracing")))]
  pub full_tracing_level: tracing::Level,
}

#[cfg(feature = "partial-tracing")]
#[cfg_attr(docsrs, doc(cfg(feature = "partial-tracing")))]
impl TracingConfig {
  pub fn new(enabled: bool) -> Self {
    Self {
      enabled,
      ..Self::default()
    }
  }
}

#[cfg(feature = "partial-tracing")]
#[cfg_attr(docsrs, doc(cfg(feature = "partial-tracing")))]
impl Default for TracingConfig {
  fn default() -> Self {
    Self {
      enabled:                                             false,
      default_tracing_level:                               tracing::Level::INFO,
      #[cfg(feature = "full-tracing")]
      full_tracing_level:                                  tracing::Level::DEBUG,
    }
  }
}

/// Configuration options for sentinel clients.
#[derive(Clone, Debug)]
#[cfg(feature = "sentinel-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
pub struct SentinelConfig {
  /// The hostname for the sentinel node.
  ///
  /// Default: `127.0.0.1`
  pub host:     String,
  /// The port on which the sentinel node is listening.
  ///
  /// Default: `26379`
  pub port:     u16,
  /// An optional ACL username for the client to use when authenticating. If ACL rules are not configured this should
  /// be `None`.
  ///
  /// Default: `None`
  pub username: Option<String>,
  /// An optional password for the client to use when authenticating.
  ///
  /// Default: `None`
  pub password: Option<String>,
  /// TLS configuration fields. If `None` the connection will not use TLS.
  ///
  /// See the `tls` examples on Github for more information.
  ///
  /// Default: `None`
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  #[cfg_attr(
    docsrs,
    doc(cfg(any(
      feature = "enable-native-tls",
      feature = "enable-rustls",
      feature = "enable-rustls-ring"
    )))
  )]
  pub tls:      Option<TlsConfig>,
  /// Whether to enable tracing for this client.
  ///
  /// Default: `false`
  #[cfg(feature = "partial-tracing")]
  #[cfg_attr(docsrs, doc(cfg(feature = "partial-tracing")))]
  pub tracing:  TracingConfig,
}

#[cfg(feature = "sentinel-client")]
impl Default for SentinelConfig {
  fn default() -> Self {
    SentinelConfig {
      host:                                        "127.0.0.1".into(),
      port:                                        26379,
      username:                                    None,
      password:                                    None,
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      tls:                                         None,
      #[cfg(feature = "partial-tracing")]
      tracing:                                     TracingConfig::default(),
    }
  }
}

#[doc(hidden)]
#[cfg(feature = "sentinel-client")]
impl From<SentinelConfig> for Config {
  fn from(config: SentinelConfig) -> Self {
    Config {
      server: ServerConfig::Centralized {
        server: Server::new(config.host, config.port),
      },
      fail_fast: true,
      database: None,
      blocking: Blocking::Block,
      username: config.username,
      password: config.password,
      version: RespVersion::RESP2,
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      tls: config.tls,
      #[cfg(feature = "partial-tracing")]
      tracing: config.tracing,
      #[cfg(feature = "mocks")]
      mocks: None,
      #[cfg(feature = "credential-provider")]
      credential_provider: None,
    }
  }
}

/// Options to configure or overwrite for individual commands.
///
/// Fields left as `None` will use the value from the corresponding client or global config option.
///
/// ```rust
/// # use fred::prelude::*;
/// async fn example() -> Result<(), Error> {
///   let options = Options {
///     max_attempts: Some(10),
///     max_redirections: Some(2),
///     ..Default::default()
///   };
///
///   let client = Client::default();
///   client.init().await?;
///   let _: () = client.with_options(&options).get("foo").await?;
///
///   Ok(())
/// }
/// ```
///
/// See [WithOptions](crate::clients::WithOptions) for more information.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct Options {
  /// Set the max number of write attempts for a command.
  pub max_attempts:     Option<u32>,
  /// Set the max number of cluster redirections to follow for a command.
  pub max_redirections: Option<u32>,
  /// Set the timeout duration for a command.
  ///
  /// This interface is more<sup>*</sup> cancellation-safe than a simple [timeout](https://docs.rs/tokio/latest/tokio/time/fn.timeout.html) call.
  ///
  /// <sup>*</sup> But it's not perfect. There's no reliable mechanism to cancel a command once it has been written
  /// to the connection.
  pub timeout:          Option<Duration>,
  /// The cluster node that should receive the command.
  ///
  /// The caller will receive a `ErrorKind::Cluster` error if the provided server does not exist.
  ///
  /// The client will still follow redirection errors via this interface. Callers may not notice this, but incorrect
  /// server arguments here could result in unnecessary calls to refresh the cached cluster routing table.
  pub cluster_node:     Option<Server>,
  /// The cluster hashing policy to use, if applicable.
  ///
  /// If `cluster_node` is also provided it will take precedence over this value.
  pub cluster_hash:     Option<ClusterHash>,
  /// Whether the command should fail quickly if the connection is not healthy or available for writes. This always
  /// takes precedence over `max_attempts` if `true`.
  ///
  /// This can be useful for caching use cases where it's preferable to fail fast with a fallback query to another
  /// storage layer rather than wait for a reconnection delay.
  ///
  /// Default: `false`
  pub fail_fast:        bool,
  /// Whether to send `CLIENT CACHING yes|no` before the command.
  #[cfg(feature = "i-tracking")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-tracking")))]
  pub caching:          Option<bool>,
}

impl Options {
  /// Set the non-null values from `other` onto `self`.
  pub fn extend(&mut self, other: &Self) -> &mut Self {
    if let Some(val) = other.max_attempts {
      self.max_attempts = Some(val);
    }
    if let Some(val) = other.max_redirections {
      self.max_redirections = Some(val);
    }
    if let Some(val) = other.timeout {
      self.timeout = Some(val);
    }
    if let Some(ref val) = other.cluster_node {
      self.cluster_node = Some(val.clone());
    }
    if let Some(ref cluster_hash) = other.cluster_hash {
      self.cluster_hash = Some(cluster_hash.clone());
    }
    self.fail_fast |= other.fail_fast;

    #[cfg(feature = "i-tracking")]
    if let Some(val) = other.caching {
      self.caching = Some(val);
    }

    self
  }

  /// Create options from a command
  #[cfg(feature = "transactions")]
  pub(crate) fn from_command(cmd: &Command) -> Self {
    Options {
      max_attempts:                           Some(cmd.attempts_remaining),
      max_redirections:                       Some(cmd.redirections_remaining),
      timeout:                                cmd.timeout_dur,
      cluster_node:                           cmd.cluster_node.clone(),
      cluster_hash:                           Some(cmd.hasher.clone()),
      fail_fast:                              cmd.fail_fast,
      #[cfg(feature = "i-tracking")]
      caching:                                cmd.caching,
    }
  }

  /// Overwrite the configuration options on the provided command.
  pub(crate) fn apply(&self, command: &mut Command) {
    command.timeout_dur = self.timeout;
    command.cluster_node = self.cluster_node.clone();
    command.fail_fast = self.fail_fast;

    #[cfg(feature = "i-tracking")]
    {
      command.caching = self.caching;
    }

    if let Some(attempts) = self.max_attempts {
      command.attempts_remaining = attempts;
    }
    if let Some(redirections) = self.max_redirections {
      command.redirections_remaining = redirections;
    }
    if let Some(ref cluster_hash) = self.cluster_hash {
      command.hasher = cluster_hash.clone();
    }
  }
}

/// An interface used to periodically scale the number of clients in a [DynamicPool](crate::clients::DynamicPool).
#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
#[async_trait]
#[rm_send_if(feature = "glommio")]
pub trait PoolScale: Debug + Send + Sync {
  /// Return the amount of clients that should be added or removed from the pool.
  ///
  /// The provided [PoolStats](crate::types::stats::PoolStats) refer to samples taken since the last call to this
  /// function.
  fn scale(&self, usage: PoolStats) -> i64;

  /// A function that will be called with the new clients after they're connected and added to the pool.
  ///
  /// This is typically used to set up event handler callbacks, logging, etc.
  async fn on_added(&self, clients: Vec<Client>) {
    debug!("Added {} clients to pool.", clients.len());
  }

  /// A function that will be called with any clients that are removed from the pool.
  ///
  /// By default, this function calls [quit](crate::interfaces::ClientLike::quit) on each client.
  async fn on_removed(&self, clients: Vec<Client>) {
    futures::future::join_all(clients.iter().map(|c| c.quit())).await;
  }

  /// A function that will be called when a client cannot be added to the pool due to an error.
  async fn on_failure(&self, error: Error) {
    warn!("Failed to add client to pool due to error: {:?}", error);
  }
}

/// A dynamic pool scaling interface that only removes idle connections.
#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
#[derive(Clone, Debug)]
pub struct RemoveIdle;

#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
#[async_trait]
impl PoolScale for RemoveIdle {
  fn scale(&self, _: PoolStats) -> i64 {
    0
  }
}

/// Configuration options for a [DynamicPool](crate::clients::DynamicPool).
#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
#[derive(Clone)]
pub struct DynamicPoolConfig {
  /// The minimum number of clients in the pool.
  ///
  /// Default: 1
  pub min_clients:   usize,
  /// The maximum number of clients in the pool.
  ///
  /// Default: 10
  pub max_clients:   usize,
  /// The max time a client can be idle before being disconnected and removed from the pool.
  ///
  /// Default: 10 min
  pub max_idle_time: Duration,
  /// An interface used to periodically scale the size of the pool.
  ///
  /// Default: [RemoveIdle](crate::types::config::RemoveIdle).
  pub scale:         Arc<dyn PoolScale>,
  /// A DNS resolver interface that will be applied to new clients when they're added to the pool.
  #[cfg(feature = "dns")]
  #[cfg_attr(docsrs, doc(cfg(feature = "dns")))]
  pub resolver:      Option<Arc<dyn Resolve>>,
}

#[cfg(feature = "dynamic-pool")]
impl Debug for DynamicPoolConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("DynamicPoolConfig")
      .field("min_clients", &self.min_clients)
      .field("max_clients", &self.max_clients)
      .field("max_idle_time", &self.max_idle_time)
      .finish()
  }
}

#[cfg(feature = "dynamic-pool")]
impl Default for DynamicPoolConfig {
  fn default() -> Self {
    DynamicPoolConfig {
      min_clients:                      1,
      max_clients:                      10,
      max_idle_time:                    Duration::from_secs(10 * 60),
      scale:                            Arc::new(RemoveIdle),
      #[cfg(feature = "dns")]
      resolver:                         None,
    }
  }
}

#[cfg(test)]
mod tests {
  #[cfg(feature = "sentinel-auth")]
  use crate::types::config::Server;
  #[allow(unused_imports)]
  use crate::{prelude::ServerConfig, types::config::Config, utils};

  #[test]
  fn should_parse_centralized_url() {
    let url = "redis://username:password@foo.com:6379/1";
    let expected = Config {
      server: ServerConfig::new_centralized("foo.com", 6379),
      database: Some(1),
      username: Some("username".into()),
      password: Some("password".into()),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_centralized(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_centralized_url_without_port() {
    let url = "redis://foo.com";
    let expected = Config {
      server: ServerConfig::new_centralized("foo.com", 6379),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_centralized(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_centralized_url_without_creds() {
    let url = "redis://foo.com:6379/1";
    let expected = Config {
      server: ServerConfig::new_centralized("foo.com", 6379),
      database: Some(1),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_centralized(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_centralized_url_without_db() {
    let url = "redis://username:password@foo.com:6379";
    let expected = Config {
      server: ServerConfig::new_centralized("foo.com", 6379),
      username: Some("username".into()),
      password: Some("password".into()),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_centralized(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "enable-native-tls")]
  fn should_parse_centralized_url_with_tls() {
    let url = "rediss://username:password@foo.com:6379/1";
    let expected = Config {
      server: ServerConfig::new_centralized("foo.com", 6379),
      database: Some(1),
      username: Some("username".into()),
      password: Some("password".into()),
      tls: utils::tls_config_from_url(true).unwrap(),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_centralized(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_clustered_url() {
    let url = "redis-cluster://username:password@foo.com:30000";
    let expected = Config {
      server: ServerConfig::new_clustered(vec![("foo.com", 30000)]),
      username: Some("username".into()),
      password: Some("password".into()),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_clustered(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_clustered_url_without_port() {
    let url = "redis-cluster://foo.com";
    let expected = Config {
      server: ServerConfig::new_clustered(vec![("foo.com", 6379)]),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_clustered(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_clustered_url_without_creds() {
    let url = "redis-cluster://foo.com:30000";
    let expected = Config {
      server: ServerConfig::new_clustered(vec![("foo.com", 30000)]),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_clustered(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_clustered_url_with_other_nodes() {
    let url = "redis-cluster://username:password@foo.com:30000?node=bar.com:30001&node=baz.com:30002";
    let expected = Config {
      // need to be careful with the array ordering here
      server: ServerConfig::new_clustered(vec![("bar.com", 30001), ("baz.com", 30002), ("foo.com", 30000)]),
      username: Some("username".into()),
      password: Some("password".into()),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_clustered(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "enable-native-tls")]
  fn should_parse_clustered_url_with_tls() {
    let url = "rediss-cluster://username:password@foo.com:30000";
    let expected = Config {
      server: ServerConfig::new_clustered(vec![("foo.com", 30000)]),
      username: Some("username".into()),
      password: Some("password".into()),
      tls: utils::tls_config_from_url(true).unwrap(),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_clustered(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_sentinel_url() {
    let url = "redis-sentinel://username:password@foo.com:26379/1?sentinelServiceName=fakename";
    let expected = Config {
      server: ServerConfig::new_sentinel(vec![("foo.com", 26379)], "fakename"),
      username: Some("username".into()),
      password: Some("password".into()),
      database: Some(1),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_sentinel(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_sentinel_url_with_other_nodes() {
    let url = "redis-sentinel://username:password@foo.com:26379/1?sentinelServiceName=fakename&node=bar.com:26380&\
               node=baz.com:26381";
    let expected = Config {
      // also need to be careful with array ordering here
      server: ServerConfig::new_sentinel(
        vec![("bar.com", 26380), ("baz.com", 26381), ("foo.com", 26379)],
        "fakename",
      ),
      username: Some("username".into()),
      password: Some("password".into()),
      database: Some(1),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_sentinel(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "unix-sockets")]
  fn should_parse_unix_socket_url_no_auth() {
    let url = "redis+unix:///path/to/redis.sock";
    let expected = Config {
      server: ServerConfig::Unix {
        path: "/path/to/redis.sock".into(),
      },
      username: None,
      password: None,
      ..Default::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_unix(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "unix-sockets")]
  fn should_parse_unix_socket_url_with_auth() {
    let url = "redis+unix://username:password@foo/path/to/redis.sock";
    let expected = Config {
      server: ServerConfig::Unix {
        path: "/path/to/redis.sock".into(),
      },
      username: Some("username".into()),
      password: Some("password".into()),
      ..Default::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_unix(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "enable-native-tls")]
  fn should_parse_sentinel_url_with_tls() {
    let url = "rediss-sentinel://username:password@foo.com:26379/1?sentinelServiceName=fakename";
    let expected = Config {
      server: ServerConfig::new_sentinel(vec![("foo.com", 26379)], "fakename"),
      username: Some("username".into()),
      password: Some("password".into()),
      database: Some(1),
      tls: utils::tls_config_from_url(true).unwrap(),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_sentinel(url).unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "sentinel-auth")]
  fn should_parse_sentinel_url_with_sentinel_auth() {
    let url = "redis-sentinel://username1:password1@foo.com:26379/1?sentinelServiceName=fakename&\
               sentinelUsername=username2&sentinelPassword=password2";
    let expected = Config {
      server: ServerConfig::Sentinel {
        hosts:        vec![Server::new("foo.com", 26379)],
        service_name: "fakename".into(),
        username:     Some("username2".into()),
        password:     Some("password2".into()),
      },
      username: Some("username1".into()),
      password: Some("password1".into()),
      database: Some(1),
      ..Config::default()
    };

    let actual = Config::from_url(url).unwrap();
    assert_eq!(actual, expected);
    let actual = Config::from_url_sentinel(url).unwrap();
    assert_eq!(actual, expected);
  }
}
