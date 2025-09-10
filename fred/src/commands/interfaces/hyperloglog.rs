use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{FromValue, Key, MultipleKeys, MultipleValues},
};
use fred_macros::rm_send_if;
use futures::Future;
use std::convert::TryInto;

/// Functions that implement the [HyperLogLog](https://redis.io/commands#hyperloglog) interface.
#[rm_send_if(feature = "glommio")]
pub trait HyperloglogInterface: ClientLike + Sized {
  /// Adds all the element arguments to the HyperLogLog data structure stored at the variable name specified as first
  /// argument.
  ///
  /// <https://redis.io/commands/pfadd>
  fn pfadd<R, K, V>(&self, key: K, elements: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(elements);
      commands::hyperloglog::pfadd(self, key, elements).await?.convert()
    }
  }

  /// When called with a single key, returns the approximated cardinality computed by the HyperLogLog data structure
  /// stored at the specified variable, which is 0 if the variable does not exist.
  ///
  /// When called with multiple keys, returns the approximated cardinality of the union of the HyperLogLogs passed, by
  /// internally merging the HyperLogLogs stored at the provided keys into a temporary HyperLogLog.
  ///
  /// <https://redis.io/commands/pfcount>
  fn pfcount<R, K>(&self, keys: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::hyperloglog::pfcount(self, keys).await?.convert()
    }
  }

  /// Merge multiple HyperLogLog values into an unique value that will approximate the cardinality of the union of the
  /// observed sets of the source HyperLogLog structures.
  ///
  /// <https://redis.io/commands/pfmerge>
  fn pfmerge<R, D, S>(&self, dest: D, sources: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    D: Into<Key> + Send,
    S: Into<MultipleKeys> + Send,
  {
    async move {
      into!(dest, sources);
      commands::hyperloglog::pfmerge(self, dest, sources).await?.convert()
    }
  }
}
