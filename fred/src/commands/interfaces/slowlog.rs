use crate::{
  commands,
  interfaces::{ClientLike, FredResult},
  types::FromValue,
};
use fred_macros::rm_send_if;
use futures::Future;

/// Functions that implement the [slowlog](https://redis.io/commands#server) interface.
#[rm_send_if(feature = "glommio")]
pub trait SlowlogInterface: ClientLike + Sized {
  /// This command is used to read the slow queries log.
  ///
  /// <https://redis.io/commands/slowlog#reading-the-slow-log>
  fn slowlog_get<R>(&self, count: Option<i64>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::slowlog::slowlog_get(self, count).await?.convert() }
  }

  /// This command is used to read length of the slow queries log.
  ///
  /// <https://redis.io/commands/slowlog#obtaining-the-current-length-of-the-slow-log>
  fn slowlog_length<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::slowlog::slowlog_length(self).await?.convert() }
  }

  /// This command is used to reset the slow queries log.
  ///
  /// <https://redis.io/commands/slowlog#resetting-the-slow-log>
  fn slowlog_reset(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::slowlog::slowlog_reset(self).await }
  }
}
