#[cfg(feature = "i-tracking")]
use crate::types::{client::Toggle, MultipleStrings};
use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{
    client::{ClientKillFilter, ClientKillType, ClientPauseKind, ClientReplyFlag},
    config::Server,
    ClientUnblockFlag,
    FromValue,
    Value,
  },
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use std::collections::HashMap;

/// Functions that implement the [client](https://redis.io/commands#connection) interface.
#[rm_send_if(feature = "glommio")]
pub trait ClientInterface: ClientLike + Sized {
  /// Return the ID of the current connection.
  ///
  /// Note: Against a clustered deployment this will return the ID of a random connection. See
  /// [connection_ids](Self::connection_ids) for  more information.
  ///
  /// <https://redis.io/commands/client-id>
  fn client_id<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::client::client_id(self).await?.convert() }
  }

  /// Read the connection IDs for the active connections to each server.
  ///
  /// The returned map contains each server's `host:port` and the result of calling `CLIENT ID` on the connection.
  ///
  /// Note: despite being async this function will return cached information from the client if possible.
  fn connection_ids(&self) -> HashMap<Server, i64> {
    self.inner().backchannel.connection_ids.lock().clone()
  }

  /// The command returns information and statistics about the current client connection in a mostly human readable
  /// format.
  ///
  /// <https://redis.io/commands/client-info>
  fn client_info<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::client::client_info(self).await?.convert() }
  }

  /// Close a given connection or set of connections.
  ///
  /// <https://redis.io/commands/client-kill>
  fn client_kill<R>(&self, filters: Vec<ClientKillFilter>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::client::client_kill(self, filters).await?.convert() }
  }

  /// The CLIENT LIST command returns information and statistics about the client connections server in a mostly human
  /// readable format.
  ///
  /// <https://redis.io/commands/client-list>
  fn client_list<R, I>(
    &self,
    r#type: Option<ClientKillType>,
    ids: Option<Vec<String>>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::client::client_list(self, r#type, ids).await?.convert() }
  }

  /// The CLIENT GETNAME returns the name of the current connection as set by CLIENT SETNAME.
  ///
  /// <https://redis.io/commands/client-getname>
  fn client_getname<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::client::client_getname(self).await?.convert() }
  }

  /// Assign a name to the current connection.
  ///
  /// **Note: The client automatically generates a unique name for each client that is shared by all underlying
  /// connections. Use `self.id() to read the automatically generated name.**
  ///
  /// <https://redis.io/commands/client-setname>
  fn client_setname<S>(&self, name: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::client::client_setname(self, name).await
    }
  }

  /// CLIENT PAUSE is a connections control command able to suspend all the Redis clients for the specified amount of
  /// time (in milliseconds).
  ///
  /// <https://redis.io/commands/client-pause>
  fn client_pause(&self, timeout: i64, mode: Option<ClientPauseKind>) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::client::client_pause(self, timeout, mode).await }
  }

  /// CLIENT UNPAUSE is used to resume command processing for all clients that were paused by CLIENT PAUSE.
  ///
  /// <https://redis.io/commands/client-unpause>
  fn client_unpause(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::client::client_unpause(self).await }
  }

  /// The CLIENT REPLY command controls whether the server will reply the client's commands. The following modes are
  /// available:
  ///
  /// <https://redis.io/commands/client-reply>
  fn client_reply(&self, flag: ClientReplyFlag) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::client::client_reply(self, flag).await }
  }

  /// This command can unblock, from a different connection, a client blocked in a blocking operation, such as for
  /// instance BRPOP or XREAD or WAIT.
  ///
  /// Note: this command is sent on a backchannel connection and will work even when the main connection is blocked.
  ///
  /// <https://redis.io/commands/client-unblock>
  fn client_unblock<R, S>(&self, id: S, flag: Option<ClientUnblockFlag>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Value> + Send,
  {
    async move {
      into!(id);
      commands::client::client_unblock(self, id, flag).await?.convert()
    }
  }

  /// A convenience function to unblock any blocked connection on this client.
  fn unblock_self(&self, flag: Option<ClientUnblockFlag>) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::client::unblock_self(self, flag).await }
  }

  /// Returns message.
  ///
  /// <https://redis.io/docs/latest/commands/echo>
  fn echo<R, M>(&self, message: M) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    M: TryInto<Value> + Send,
    M::Error: Into<Error> + Send,
  {
    async move {
      try_into!(message);
      commands::client::echo(self, message).await?.convert()
    }
  }

  /// This command enables the tracking feature of the Redis server that is used for server assisted client side
  /// caching.
  ///
  /// <https://redis.io/commands/client-tracking/>
  ///
  /// This function is designed to work against a specific server, either via a centralized server config or
  /// [with_options](crate::interfaces::ClientLike::with_options). See
  /// [crate::interfaces::TrackingInterface::start_tracking] for a version that works with all server deployment
  /// modes.
  #[cfg(feature = "i-tracking")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-tracking")))]
  fn client_tracking<R, T, P>(
    &self,
    toggle: T,
    redirect: Option<i64>,
    prefixes: P,
    bcast: bool,
    optin: bool,
    optout: bool,
    noloop: bool,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    T: TryInto<Toggle> + Send,
    T::Error: Into<Error> + Send,
    P: Into<MultipleStrings> + Send,
  {
    async move {
      try_into!(toggle);
      into!(prefixes);
      commands::tracking::client_tracking(self, toggle, redirect, prefixes, bcast, optin, optout, noloop)
        .await?
        .convert()
    }
  }

  /// The command returns information about the current client connection's use of the server assisted client side
  /// caching feature.
  ///
  /// <https://redis.io/commands/client-trackinginfo/>
  #[cfg(feature = "i-tracking")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-tracking")))]
  fn client_trackinginfo<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::tracking::client_trackinginfo(self).await?.convert() }
  }

  /// This command returns the client ID we are redirecting our tracking notifications to.
  ///
  /// <https://redis.io/commands/client-getredir/>
  #[cfg(feature = "i-tracking")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-tracking")))]
  fn client_getredir<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::tracking::client_getredir(self).await?.convert() }
  }

  /// This command controls the tracking of the keys in the next command executed by the connection, when tracking is
  /// enabled in OPTIN or OPTOUT mode.
  ///
  /// <https://redis.io/commands/client-caching/>
  ///
  /// This function is designed to work against a specific server. See
  /// [with_options](crate::interfaces::ClientLike::with_options) for a variation that works with all deployment
  /// types.
  #[cfg(feature = "i-tracking")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-tracking")))]
  fn client_caching<R>(&self, enabled: bool) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::tracking::client_caching(self, enabled).await?.convert() }
  }
}
