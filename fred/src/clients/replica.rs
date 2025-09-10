use crate::{
  clients::{Client, Pipeline},
  error::Error,
  interfaces::{self, *},
  modules::inner::ClientInner,
  protocol::command::{Command, RouterCommand},
  runtime::{oneshot_channel, RefCount},
  types::config::Server,
};
use std::{collections::HashMap, fmt, fmt::Formatter};

/// A struct for interacting with cluster replica nodes.
///
/// All commands sent via this interface will use a replica node, if possible. The underlying connections are shared
/// with the main client in order to maintain an up-to-date view of the system in the event that replicas change or
/// are promoted. The cached replica routing table will be updated on the client when following cluster redirections
/// or when any connection closes.
///
/// [Redis replication is asynchronous](https://redis.io/docs/management/replication/).
#[derive(Clone)]
#[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
pub struct Replicas<C: ClientLike> {
  pub(crate) client: C,
}

impl<C: ClientLike> fmt::Debug for Replicas<C> {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    f.debug_struct("Replicas").field("id", &self.client.inner().id).finish()
  }
}

#[doc(hidden)]
impl From<&RefCount<ClientInner>> for Replicas<Client> {
  fn from(inner: &RefCount<ClientInner>) -> Self {
    Replicas {
      client: Client::from(inner),
    }
  }
}

impl<C: ClientLike> ClientLike for Replicas<C> {
  #[doc(hidden)]
  fn inner(&self) -> &RefCount<ClientInner> {
    self.client.inner()
  }

  #[doc(hidden)]
  fn change_command(&self, command: &mut Command) {
    command.use_replica = true;
    self.client.change_command(command);
  }

  #[doc(hidden)]
  fn send_command<T>(&self, command: T) -> Result<(), Error>
  where
    T: Into<Command>,
  {
    self.client.send_command(command)
  }
}

#[cfg(feature = "i-redis-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
impl<C: ClientLike> RedisJsonInterface for Replicas<C> {}
#[cfg(feature = "i-time-series")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
impl<C: ClientLike> TimeSeriesInterface for Replicas<C> {}
#[cfg(feature = "i-cluster")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-cluster")))]
impl<C: ClientLike> ClusterInterface for Replicas<C> {}
#[cfg(feature = "i-config")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-config")))]
impl<C: ClientLike> ConfigInterface for Replicas<C> {}
#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
impl<C: ClientLike> GeoInterface for Replicas<C> {}
#[cfg(feature = "i-hashes")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hashes")))]
impl<C: ClientLike> HashesInterface for Replicas<C> {}
#[cfg(feature = "i-hyperloglog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hyperloglog")))]
impl<C: ClientLike> HyperloglogInterface for Replicas<C> {}
#[cfg(feature = "i-keys")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-keys")))]
impl<C: ClientLike> KeysInterface for Replicas<C> {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl<C: ClientLike> LuaInterface for Replicas<C> {}
#[cfg(feature = "i-lists")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-lists")))]
impl<C: ClientLike> ListInterface for Replicas<C> {}
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl<C: ClientLike> MemoryInterface for Replicas<C> {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl<C: ClientLike> ServerInterface for Replicas<C> {}
#[cfg(feature = "i-slowlog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-slowlog")))]
impl<C: ClientLike> SlowlogInterface for Replicas<C> {}
#[cfg(feature = "i-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sets")))]
impl<C: ClientLike> SetsInterface for Replicas<C> {}
#[cfg(feature = "i-sorted-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sorted-sets")))]
impl<C: ClientLike> SortedSetsInterface for Replicas<C> {}
#[cfg(feature = "i-streams")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
impl<C: ClientLike> StreamsInterface for Replicas<C> {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl<C: ClientLike> FunctionInterface for Replicas<C> {}
#[cfg(feature = "i-redisearch")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
impl<C: ClientLike> RediSearchInterface for Replicas<C> {}

impl<C: ClientLike> Replicas<C> {
  /// Read a mapping of replica server IDs to primary server IDs.
  pub fn nodes(&self) -> HashMap<Server, Server> {
    self.client.inner().server_state.read().replicas.clone()
  }

  /// Send a series of commands in a [pipeline](https://redis.io/docs/manual/pipelining/).
  pub fn pipeline(&self) -> Pipeline<Replicas<C>> {
    Pipeline::from(self.clone())
  }

  /// Read the underlying [RedisClient](crate::clients::Client) that interacts with primary nodes.
  pub fn client(&self) -> Client {
    Client::from(self.client.inner())
  }

  /// Sync the cached replica routing table with the server(s).
  ///
  /// If `reset: true` the client will forcefully disconnect from replicas even if the connections could otherwise be
  /// reused.
  pub async fn sync(&self, reset: bool) -> Result<(), Error> {
    let (tx, rx) = oneshot_channel();
    let cmd = RouterCommand::SyncReplicas { tx, reset };
    interfaces::send_to_router(self.client.inner(), cmd)?;
    rx.await?
  }
}
