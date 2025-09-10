use crate::{
  interfaces::*,
  modules::inner::ClientInner,
  runtime::RefCount,
  types::config::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, SentinelConfig},
};
use std::fmt;

/// A struct for interacting directly with Sentinel nodes.
///
/// This struct **will not** communicate with Redis servers behind the sentinel interface, but rather with the
/// sentinel nodes themselves. Callers should use the [RedisClient](crate::clients::Client) interface with a
/// [ServerConfig::Sentinel](crate::types::config::ServerConfig::Sentinel) for interacting with Redis services behind
/// a sentinel layer.
///
/// See the [sentinel API docs](https://redis.io/topics/sentinel#sentinel-api) for more information.
#[derive(Clone)]
#[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
pub struct SentinelClient {
  inner: RefCount<ClientInner>,
}

impl ClientLike for SentinelClient {
  #[doc(hidden)]
  fn inner(&self) -> &RefCount<ClientInner> {
    &self.inner
  }
}

impl fmt::Debug for SentinelClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SentinelClient")
      .field("id", &self.inner.id)
      .field("state", &self.state())
      .finish()
  }
}

#[doc(hidden)]
impl<'a> From<&'a RefCount<ClientInner>> for SentinelClient {
  fn from(inner: &'a RefCount<ClientInner>) -> Self {
    SentinelClient { inner: inner.clone() }
  }
}

impl EventInterface for SentinelClient {}
impl SentinelInterface for SentinelClient {}
impl MetricsInterface for SentinelClient {}
#[cfg(feature = "i-acl")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-acl")))]
impl AclInterface for SentinelClient {}
#[cfg(feature = "i-pubsub")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-pubsub")))]
impl PubsubInterface for SentinelClient {}
#[cfg(feature = "i-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-client")))]
impl ClientInterface for SentinelClient {}
impl AuthInterface for SentinelClient {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl HeartbeatInterface for SentinelClient {}

impl SentinelClient {
  /// Create a new client instance without connecting to the sentinel node.
  ///
  /// See the [builder](crate::types::Builder) interface for more information.
  pub fn new(
    config: SentinelConfig,
    perf: Option<PerformanceConfig>,
    connection: Option<ConnectionConfig>,
    policy: Option<ReconnectPolicy>,
  ) -> SentinelClient {
    SentinelClient {
      inner: ClientInner::new(
        config.into(),
        perf.unwrap_or_default(),
        connection.unwrap_or_default(),
        policy,
      ),
    }
  }
}
