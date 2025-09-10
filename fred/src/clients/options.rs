use crate::{
  error::Error,
  interfaces::*,
  modules::inner::ClientInner,
  protocol::command::Command,
  runtime::RefCount,
  types::config::Options,
};
use std::{fmt, ops::Deref};

#[cfg(feature = "replicas")]
use crate::clients::Replicas;

/// A client interface used to customize command configuration options.
///
/// See [Options](crate::types::config::Options) for more information.
///
/// ```rust
/// # use fred::prelude::*;
/// # use std::time::Duration;
/// async fn example() -> Result<(), Error> {
///   let client = Client::default();
///   client.init().await?;
///
///   let options = Options {
///     max_redirections: Some(3),
///     max_attempts: Some(1),
///     timeout: Some(Duration::from_secs(10)),
///     ..Default::default()
///   };
///   let foo: Option<String> = client.with_options(&options).get("foo").await?;
///
///   // reuse the options bindings
///   let with_options = client.with_options(&options);
///   let foo: () = with_options.get("foo").await?;
///   let bar: () = with_options.get("bar").await?;
///
///   // combine with other client types
///   let pipeline = client.pipeline().with_options(&options);
///   let _: () = pipeline.get("foo").await?;
///   let _: () = pipeline.get("bar").await?;
///   // custom options will be applied to each command
///   println!("results: {:?}", pipeline.all::<i64>().await?);
///
///   Ok(())
/// }
/// ```
#[derive(Clone)]
pub struct WithOptions<C: ClientLike> {
  pub(crate) client:  C,
  pub(crate) options: Options,
}

impl<C: ClientLike> WithOptions<C> {
  /// Read the options that will be applied to commands.
  pub fn options(&self) -> &Options {
    &self.options
  }

  /// Create a client that interacts with replica nodes.
  #[cfg(feature = "replicas")]
  #[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
  pub fn replicas(&self) -> Replicas<WithOptions<C>> {
    Replicas { client: self.clone() }
  }
}

impl<C: ClientLike> Deref for WithOptions<C> {
  type Target = C;

  fn deref(&self) -> &Self::Target {
    &self.client
  }
}

impl<C: ClientLike> fmt::Debug for WithOptions<C> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WithOptions")
      .field("client", &self.client.id())
      .field("options", &self.options)
      .finish()
  }
}

impl<C: ClientLike> ClientLike for WithOptions<C> {
  #[doc(hidden)]
  fn inner(&self) -> &RefCount<ClientInner> {
    self.client.inner()
  }

  #[doc(hidden)]
  fn change_command(&self, command: &mut Command) {
    self.client.change_command(command);
    self.options.apply(command);
  }

  #[doc(hidden)]
  fn send_command<T>(&self, command: T) -> Result<(), Error>
  where
    T: Into<Command>,
  {
    let mut command: Command = command.into();
    self.options.apply(&mut command);
    self.client.send_command(command)
  }
}

#[cfg(feature = "i-acl")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-acl")))]
impl<C: AclInterface> AclInterface for WithOptions<C> {}
#[cfg(feature = "i-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-client")))]
impl<C: ClientInterface> ClientInterface for WithOptions<C> {}
#[cfg(feature = "i-cluster")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-cluster")))]
impl<C: ClusterInterface> ClusterInterface for WithOptions<C> {}
#[cfg(feature = "i-pubsub")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-pubsub")))]
impl<C: PubsubInterface> PubsubInterface for WithOptions<C> {}
#[cfg(feature = "i-config")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-config")))]
impl<C: ConfigInterface> ConfigInterface for WithOptions<C> {}
#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
impl<C: GeoInterface> GeoInterface for WithOptions<C> {}
#[cfg(feature = "i-hashes")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hashes")))]
impl<C: HashesInterface> HashesInterface for WithOptions<C> {}
#[cfg(feature = "i-hyperloglog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hyperloglog")))]
impl<C: HyperloglogInterface> HyperloglogInterface for WithOptions<C> {}
#[cfg(feature = "i-keys")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-keys")))]
impl<C: KeysInterface> KeysInterface for WithOptions<C> {}
#[cfg(feature = "i-lists")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-lists")))]
impl<C: ListInterface> ListInterface for WithOptions<C> {}
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl<C: MemoryInterface> MemoryInterface for WithOptions<C> {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl<C: AuthInterface> AuthInterface for WithOptions<C> {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl<C: ServerInterface> ServerInterface for WithOptions<C> {}
#[cfg(feature = "i-slowlog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-slowlog")))]
impl<C: SlowlogInterface> SlowlogInterface for WithOptions<C> {}
#[cfg(feature = "i-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sets")))]
impl<C: SetsInterface> SetsInterface for WithOptions<C> {}
#[cfg(feature = "i-sorted-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sorted-sets")))]
impl<C: SortedSetsInterface> SortedSetsInterface for WithOptions<C> {}
#[cfg(feature = "i-streams")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
impl<C: StreamsInterface> StreamsInterface for WithOptions<C> {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl<C: FunctionInterface> FunctionInterface for WithOptions<C> {}
#[cfg(feature = "i-redis-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
impl<C: RedisJsonInterface> RedisJsonInterface for WithOptions<C> {}
#[cfg(feature = "i-time-series")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
impl<C: TimeSeriesInterface> TimeSeriesInterface for WithOptions<C> {}
#[cfg(feature = "i-redisearch")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
impl<C: RediSearchInterface> RediSearchInterface for WithOptions<C> {}
