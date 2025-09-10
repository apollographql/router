use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{FromValue, Key, MultipleKeys, MultipleValues, Value},
};
use fred_macros::rm_send_if;
use futures::Future;
use std::convert::TryInto;

/// Functions that implement the [sets](https://redis.io/commands#set) interface.
#[rm_send_if(feature = "glommio")]
pub trait SetsInterface: ClientLike + Sized {
  /// Add the specified members to the set stored at `key`.
  ///
  /// <https://redis.io/commands/sadd>
  fn sadd<R, K, V>(&self, key: K, members: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(members);
      commands::sets::sadd(self, key, members).await?.convert()
    }
  }

  /// Returns the set cardinality (number of elements) of the set stored at `key`.
  ///
  /// <https://redis.io/commands/scard>
  fn scard<R, K>(&self, key: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::sets::scard(self, key).await?.convert()
    }
  }

  /// Returns the members of the set resulting from the difference between the first set and all the successive sets.
  ///
  /// <https://redis.io/commands/sdiff>
  fn sdiff<R, K>(&self, keys: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::sets::sdiff(self, keys).await?.convert()
    }
  }

  /// This command is equal to SDIFF, but instead of returning the resulting set, it is stored in `destination`.
  ///
  /// <https://redis.io/commands/sdiffstore>
  fn sdiffstore<R, D, K>(&self, dest: D, keys: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    D: Into<Key> + Send,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(dest, keys);
      commands::sets::sdiffstore(self, dest, keys).await?.convert()
    }
  }

  /// Returns the members of the set resulting from the intersection of all the given sets.
  ///
  /// <https://redis.io/commands/sinter>
  fn sinter<R, K>(&self, keys: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::sets::sinter(self, keys).await?.convert()
    }
  }

  /// This command is equal to SINTER, but instead of returning the resulting set, it is stored in `destination`.
  ///
  /// <https://redis.io/commands/sinterstore>
  fn sinterstore<R, D, K>(&self, dest: D, keys: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    D: Into<Key> + Send,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(dest, keys);
      commands::sets::sinterstore(self, dest, keys).await?.convert()
    }
  }

  /// Returns if `member` is a member of the set stored at `key`.
  ///
  /// <https://redis.io/commands/sismember>
  fn sismember<R, K, V>(&self, key: K, member: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(member);
      commands::sets::sismember(self, key, member).await?.convert()
    }
  }

  /// Returns whether each member is a member of the set stored at `key`.
  ///
  /// <https://redis.io/commands/smismember>
  fn smismember<R, K, V>(&self, key: K, members: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(members);
      commands::sets::smismember(self, key, members).await?.convert()
    }
  }

  /// Returns all the members of the set value stored at `key`.
  ///
  /// <https://redis.io/commands/smembers>
  fn smembers<R, K>(&self, key: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::sets::smembers(self, key).await?.convert()
    }
  }

  /// Move `member` from the set at `source` to the set at `destination`.
  ///
  /// <https://redis.io/commands/smove>
  fn smove<R, S, D, V>(&self, source: S, dest: D, member: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Key> + Send,
    D: Into<Key> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(source, dest);
      try_into!(member);
      commands::sets::smove(self, source, dest, member).await?.convert()
    }
  }

  /// Removes and returns one or more random members from the set value store at `key`.
  ///
  /// <https://redis.io/commands/spop>
  fn spop<R, K>(&self, key: K, count: Option<usize>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::sets::spop(self, key, count).await?.convert()
    }
  }

  /// When called with just the key argument, return a random element from the set value stored at `key`.
  ///
  /// If the provided `count` argument is positive, return an array of distinct elements. The array's length is either
  /// count or the set's cardinality (SCARD), whichever is lower.
  ///
  /// <https://redis.io/commands/srandmember>
  fn srandmember<R, K>(&self, key: K, count: Option<usize>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::sets::srandmember(self, key, count).await?.convert()
    }
  }

  /// Remove the specified members from the set stored at `key`.
  ///
  /// <https://redis.io/commands/srem>
  fn srem<R, K, V>(&self, key: K, members: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(members);
      commands::sets::srem(self, key, members).await?.convert()
    }
  }

  /// Returns the members of the set resulting from the union of all the given sets.
  ///
  /// <https://redis.io/commands/sunion>
  fn sunion<R, K>(&self, keys: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::sets::sunion(self, keys).await?.convert()
    }
  }

  /// This command is equal to SUNION, but instead of returning the resulting set, it is stored in `destination`.
  ///
  /// <https://redis.io/commands/sunionstore>
  fn sunionstore<R, D, K>(&self, dest: D, keys: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    D: Into<Key> + Send,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(dest, keys);
      commands::sets::sunionstore(self, dest, keys).await?.convert()
    }
  }
}
