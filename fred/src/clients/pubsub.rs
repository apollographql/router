use crate::{
  commands,
  error::Error,
  interfaces::*,
  modules::inner::ClientInner,
  prelude::Client,
  runtime::{spawn, JoinHandle, RefCount, RwLock},
  types::{
    config::{Config, ConnectionConfig, PerformanceConfig, ReconnectPolicy},
    Key,
    MultipleStrings,
  },
  util::group_by_hash_slot,
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use std::{collections::BTreeSet, fmt, fmt::Formatter, future::Future, mem};

type ChannelSet = RefCount<RwLock<BTreeSet<Str>>>;

/// A subscriber client that will manage subscription state to any [pubsub](https://redis.io/docs/manual/pubsub/) channels or patterns for the caller.
///
/// If the connection to the server closes for any reason this struct can automatically re-subscribe to channels,
/// patterns, and sharded channels.
///
/// **Note: most non-pubsub commands are only supported when using RESP3.**
///
/// ```rust no_run
/// use fred::clients::SubscriberClient;
/// use fred::prelude::*;
///
/// async fn example() -> Result<(), Error> {
///   let subscriber = Builder::default_centralized().build_subscriber_client()?;
///   subscriber.init().await?;
///
///   // spawn a task that will re-subscribe to channels and patterns after reconnecting
///   let _ = subscriber.manage_subscriptions();
///
///   let mut message_rx = subscriber.message_rx();
///   let jh = tokio::spawn(async move {
///     while let Ok(message) = message_rx.recv().await {
///       println!("Recv message {:?} on channel {}", message.value, message.channel);
///     }
///   });
///
///   let _ = subscriber.subscribe("foo").await?;
///   let _ = subscriber.psubscribe("bar*").await?;
///   println!("Tracking channels: {:?}", subscriber.tracked_channels()); // foo
///   println!("Tracking patterns: {:?}", subscriber.tracked_patterns()); // bar*
///
///   // force a re-subscription
///   subscriber.resubscribe_all().await?;
///   // clear all the local state and unsubscribe
///   subscriber.unsubscribe_all().await?;
///   subscriber.quit().await?;
///   Ok(())
/// }
/// ```
#[derive(Clone)]
#[cfg_attr(docsrs, doc(cfg(feature = "subscriber-client")))]
pub struct SubscriberClient {
  channels:       ChannelSet,
  patterns:       ChannelSet,
  shard_channels: ChannelSet,
  inner:          RefCount<ClientInner>,
}

impl fmt::Debug for SubscriberClient {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    f.debug_struct("SubscriberClient")
      .field("id", &self.inner.id)
      .field("channels", &self.tracked_channels())
      .field("patterns", &self.tracked_patterns())
      .field("shard_channels", &self.tracked_shard_channels())
      .finish()
  }
}

impl ClientLike for SubscriberClient {
  #[doc(hidden)]
  fn inner(&self) -> &RefCount<ClientInner> {
    &self.inner
  }
}

impl EventInterface for SubscriberClient {}
#[cfg(feature = "i-acl")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-acl")))]
impl AclInterface for SubscriberClient {}
#[cfg(feature = "i-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-client")))]
impl ClientInterface for SubscriberClient {}
#[cfg(feature = "i-cluster")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-cluster")))]
impl ClusterInterface for SubscriberClient {}
#[cfg(feature = "i-config")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-config")))]
impl ConfigInterface for SubscriberClient {}
#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
impl GeoInterface for SubscriberClient {}
#[cfg(feature = "i-hashes")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hashes")))]
impl HashesInterface for SubscriberClient {}
#[cfg(feature = "i-hyperloglog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hyperloglog")))]
impl HyperloglogInterface for SubscriberClient {}
impl MetricsInterface for SubscriberClient {}
#[cfg(feature = "transactions")]
#[cfg_attr(docsrs, doc(cfg(feature = "transactions")))]
impl TransactionInterface for SubscriberClient {}
#[cfg(feature = "i-keys")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-keys")))]
impl KeysInterface for SubscriberClient {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl LuaInterface for SubscriberClient {}
#[cfg(feature = "i-lists")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-lists")))]
impl ListInterface for SubscriberClient {}
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl MemoryInterface for SubscriberClient {}
impl AuthInterface for SubscriberClient {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl ServerInterface for SubscriberClient {}
#[cfg(feature = "i-slowlog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-slowlog")))]
impl SlowlogInterface for SubscriberClient {}
#[cfg(feature = "i-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sets")))]
impl SetsInterface for SubscriberClient {}
#[cfg(feature = "i-sorted-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sorted-sets")))]
impl SortedSetsInterface for SubscriberClient {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl HeartbeatInterface for SubscriberClient {}
#[cfg(feature = "i-streams")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
impl StreamsInterface for SubscriberClient {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl FunctionInterface for SubscriberClient {}
#[cfg(feature = "i-redis-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
impl RedisJsonInterface for SubscriberClient {}
#[cfg(feature = "i-time-series")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
impl TimeSeriesInterface for SubscriberClient {}
#[cfg(feature = "i-tracking")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-tracking")))]
impl TrackingInterface for SubscriberClient {}
#[cfg(feature = "i-redisearch")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
impl RediSearchInterface for SubscriberClient {}

#[cfg(feature = "i-pubsub")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-pubsub")))]
#[rm_send_if(feature = "glommio")]
impl PubsubInterface for SubscriberClient {
  fn subscribe<S>(&self, channels: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    into!(channels);

    async move {
      let result = commands::pubsub::subscribe(self, channels.clone()).await;
      if result.is_ok() {
        let mut guard = self.channels.write();

        for channel in channels.inner().into_iter() {
          if let Some(channel) = channel.as_bytes_str() {
            guard.insert(channel);
          }
        }
      }

      result
    }
  }

  fn unsubscribe<S>(&self, channels: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    into!(channels);

    async move {
      let result = commands::pubsub::unsubscribe(self, channels.clone()).await;
      if result.is_ok() {
        let mut guard = self.channels.write();

        if channels.len() == 0 {
          guard.clear();
        } else {
          for channel in channels.inner().into_iter() {
            if let Some(channel) = channel.as_bytes_str() {
              let _ = guard.remove(&channel);
            }
          }
        }
      }
      result
    }
  }

  fn psubscribe<S>(&self, patterns: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    into!(patterns);

    async move {
      let result = commands::pubsub::psubscribe(self, patterns.clone()).await;
      if result.is_ok() {
        let mut guard = self.patterns.write();

        for pattern in patterns.inner().into_iter() {
          if let Some(pattern) = pattern.as_bytes_str() {
            guard.insert(pattern);
          }
        }
      }
      result
    }
  }

  fn punsubscribe<S>(&self, patterns: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    into!(patterns);

    async move {
      let result = commands::pubsub::punsubscribe(self, patterns.clone()).await;
      if result.is_ok() {
        let mut guard = self.patterns.write();

        if patterns.len() == 0 {
          guard.clear();
        } else {
          for pattern in patterns.inner().into_iter() {
            if let Some(pattern) = pattern.as_bytes_str() {
              let _ = guard.remove(&pattern);
            }
          }
        }
      }
      result
    }
  }

  fn ssubscribe<C>(&self, channels: C) -> impl Future<Output = FredResult<()>> + Send
  where
    C: Into<MultipleStrings> + Send,
  {
    into!(channels);

    async move {
      let result = commands::pubsub::ssubscribe(self, channels.clone()).await;
      if result.is_ok() {
        let mut guard = self.shard_channels.write();

        for channel in channels.inner().into_iter() {
          if let Some(channel) = channel.as_bytes_str() {
            guard.insert(channel);
          }
        }
      }
      result
    }
  }

  fn sunsubscribe<C>(&self, channels: C) -> impl Future<Output = FredResult<()>> + Send
  where
    C: Into<MultipleStrings> + Send,
  {
    into!(channels);

    async move {
      let result = commands::pubsub::sunsubscribe(self, channels.clone()).await;
      if result.is_ok() {
        let mut guard = self.shard_channels.write();

        if channels.len() == 0 {
          guard.clear();
        } else {
          for channel in channels.inner().into_iter() {
            if let Some(channel) = channel.as_bytes_str() {
              let _ = guard.remove(&channel);
            }
          }
        }
      }
      result
    }
  }
}

impl SubscriberClient {
  /// Create a new client instance without connecting to the server.
  ///
  /// See the [builder](crate::types::Builder) interface for more information.
  pub fn new(
    config: Config,
    perf: Option<PerformanceConfig>,
    connection: Option<ConnectionConfig>,
    policy: Option<ReconnectPolicy>,
  ) -> SubscriberClient {
    SubscriberClient {
      channels:       RefCount::new(RwLock::new(BTreeSet::new())),
      patterns:       RefCount::new(RwLock::new(BTreeSet::new())),
      shard_channels: RefCount::new(RwLock::new(BTreeSet::new())),
      inner:          ClientInner::new(config, perf.unwrap_or_default(), connection.unwrap_or_default(), policy),
    }
  }

  /// Create a new `SubscriberClient` from the config provided to this client.
  ///
  /// The returned client will not be connected to the server, and it will use new connections after connecting.
  /// However, it will manage the same channel subscriptions as the original client.
  pub fn clone_new(&self) -> Self {
    let inner = ClientInner::new(
      self.inner.config.as_ref().clone(),
      self.inner.performance_config(),
      self.inner.connection.as_ref().clone(),
      self.inner.reconnect_policy(),
    );

    SubscriberClient {
      inner,
      channels: RefCount::new(RwLock::new(self.channels.read().clone())),
      patterns: RefCount::new(RwLock::new(self.patterns.read().clone())),
      shard_channels: RefCount::new(RwLock::new(self.shard_channels.read().clone())),
    }
  }

  /// Spawn a task that will automatically re-subscribe to any channels or channel patterns used by the client.
  pub fn manage_subscriptions(&self) -> JoinHandle<()> {
    let _self = self.clone();
    spawn(async move {
      #[allow(unused_mut)]
      let mut stream = _self.reconnect_rx();

      while let Ok(_) = stream.recv().await {
        if let Err(error) = _self.resubscribe_all().await {
          error!(
            "{}: Failed to resubscribe to channels or patterns: {:?}",
            _self.id(),
            error
          );
        }
      }
    })
  }

  /// Read the set of channels that this client will manage.
  pub fn tracked_channels(&self) -> BTreeSet<Str> {
    self.channels.read().clone()
  }

  /// Read the set of channel patterns that this client will manage.
  pub fn tracked_patterns(&self) -> BTreeSet<Str> {
    self.patterns.read().clone()
  }

  /// Read the set of shard channels that this client will manage.
  pub fn tracked_shard_channels(&self) -> BTreeSet<Str> {
    self.shard_channels.read().clone()
  }

  /// Re-subscribe to any tracked channels and patterns.
  ///
  /// This can be used to sync the client's subscriptions with the server after calling `QUIT`, then `connect`, etc.
  pub async fn resubscribe_all(&self) -> Result<(), Error> {
    let channels: Vec<Key> = self.tracked_channels().into_iter().map(|s| s.into()).collect();
    let patterns: Vec<Key> = self.tracked_patterns().into_iter().map(|s| s.into()).collect();
    let shard_channels: Vec<Key> = self.tracked_shard_channels().into_iter().map(|s| s.into()).collect();

    self.subscribe(channels).await?;
    self.psubscribe(patterns).await?;

    let shard_channel_groups = group_by_hash_slot(shard_channels)?;
    for (_, keys) in shard_channel_groups.into_iter() {
      // the client never pipelines this so no point in using join! or a pipeline here
      self.ssubscribe(keys).await?;
    }

    Ok(())
  }

  /// Unsubscribe from all tracked channels and patterns, and remove them from the client cache.
  pub async fn unsubscribe_all(&self) -> Result<(), Error> {
    let channels: Vec<Key> = mem::take(&mut *self.channels.write())
      .into_iter()
      .map(|s| s.into())
      .collect();
    let patterns: Vec<Key> = mem::take(&mut *self.patterns.write())
      .into_iter()
      .map(|s| s.into())
      .collect();
    let shard_channels: Vec<Key> = mem::take(&mut *self.shard_channels.write())
      .into_iter()
      .map(|s| s.into())
      .collect();

    self.unsubscribe(channels).await?;
    self.punsubscribe(patterns).await?;

    let shard_channel_groups = group_by_hash_slot(shard_channels)?;
    let shard_subscriptions: Vec<_> = shard_channel_groups
      .into_iter()
      .map(|(_, keys)| async { self.sunsubscribe(keys).await })
      .collect();

    futures::future::try_join_all(shard_subscriptions).await?;
    Ok(())
  }

  /// Create a new `RedisClient`, reusing the existing connection(s).
  ///
  /// Note: most non-pubsub commands are only supported when using RESP3.
  pub fn to_client(&self) -> Client {
    Client::from(&self.inner)
  }
}
