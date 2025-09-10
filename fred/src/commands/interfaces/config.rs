use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{FromValue, Value},
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use std::convert::TryInto;

/// Functions that implement the [config](https://redis.io/commands#server) interface.
#[rm_send_if(feature = "glommio")]
pub trait ConfigInterface: ClientLike + Sized {
  /// Resets the statistics reported by Redis using the INFO command.
  ///
  /// <https://redis.io/commands/config-resetstat>
  fn config_resetstat(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::config::config_resetstat(self).await }
  }

  /// The CONFIG REWRITE command rewrites the redis.conf file the server was started with, applying the minimal
  /// changes needed to make it reflect the configuration currently used by the server, which may be different
  /// compared to the original one because of the use of the CONFIG SET command.
  ///
  /// <https://redis.io/commands/config-rewrite>
  fn config_rewrite(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::config::config_rewrite(self).await }
  }

  /// The CONFIG GET command is used to read the configuration parameters of a running Redis server.
  ///
  /// <https://redis.io/commands/config-get>
  fn config_get<R, S>(&self, parameter: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(parameter);
      commands::config::config_get(self, parameter).await?.convert()
    }
  }

  /// The CONFIG SET command is used in order to reconfigure the server at run time without the need to restart Redis.
  ///
  /// <https://redis.io/commands/config-set>
  fn config_set<P, V>(&self, parameter: P, value: V) -> impl Future<Output = FredResult<()>> + Send
  where
    P: Into<Str> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(parameter);
      try_into!(value);
      commands::config::config_set(self, parameter, value).await
    }
  }
}
