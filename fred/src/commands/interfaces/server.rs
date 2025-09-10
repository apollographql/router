use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{config::Server, FromValue, Value},
};
use fred_macros::rm_send_if;
use futures::Future;

/// Functions that implement the [server](https://redis.io/commands#server) interface.
#[rm_send_if(feature = "glommio")]
pub trait ServerInterface: ClientLike {
  /// Instruct Redis to start an Append Only File rewrite process.
  ///
  /// <https://redis.io/commands/bgrewriteaof>
  fn bgrewriteaof<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::server::bgrewriteaof(self).await?.convert() }
  }

  /// Save the DB in background.
  ///
  /// <https://redis.io/commands/bgsave>
  fn bgsave<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::server::bgsave(self).await?.convert() }
  }

  /// Return the number of keys in the selected database.
  ///
  /// <https://redis.io/commands/dbsize>
  fn dbsize<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::server::dbsize(self).await?.convert() }
  }

  /// Select the database this client should use.
  ///
  /// <https://redis.io/commands/select>
  fn select<I>(&self, index: I) -> impl Future<Output = FredResult<()>> + Send
  where
    I: TryInto<Value> + Send,
    I::Error: Into<Error> + Send,
  {
    async move {
      try_into!(index);
      commands::server::select(self, index).await?.convert()
    }
  }

  /// This command will start a coordinated failover between the currently-connected-to master and one of its
  /// replicas.
  ///
  /// <https://redis.io/commands/failover>
  fn failover(
    &self,
    to: Option<(String, u16)>,
    force: bool,
    abort: bool,
    timeout: Option<u32>,
  ) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::server::failover(self, to, force, abort, timeout).await }
  }

  /// Return the UNIX TIME of the last DB save executed with success.
  ///
  /// <https://redis.io/commands/lastsave>
  fn lastsave<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::server::lastsave(self).await?.convert() }
  }

  /// This command blocks the current client until all the previous write commands are successfully transferred and
  /// acknowledged by at least the specified number of replicas. If the timeout, specified in milliseconds, is
  /// reached, the command returns even if the specified number of replicas were not yet reached.
  ///
  /// <https://redis.io/commands/wait/>
  fn wait<R>(&self, numreplicas: i64, timeout: i64) -> impl Future<Output = Result<R, Error>> + Send
  where
    R: FromValue,
  {
    async move { commands::server::wait(self, numreplicas, timeout).await?.convert() }
  }

  /// Read the primary Redis server identifier returned from the sentinel nodes.
  fn sentinel_primary(&self) -> Option<Server> {
    self.inner().server_state.read().kind.sentinel_primary()
  }

  /// Read the set of known sentinel nodes.
  fn sentinel_nodes(&self) -> Option<Vec<Server>> {
    let inner = self.inner();
    inner.server_state.read().kind.read_sentinel_nodes(&inner.config.server)
  }
}
