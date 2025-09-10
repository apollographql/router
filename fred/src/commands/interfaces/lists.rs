use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{
    lists::{LMoveDirection, ListLocation},
    FromValue,
    Key,
    Limit,
    MultipleKeys,
    MultipleStrings,
    MultipleValues,
    SortOrder,
    Value,
  },
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use std::convert::TryInto;

/// Functions that implement the [lists](https://redis.io/commands#lists) interface.
#[rm_send_if(feature = "glommio")]
pub trait ListInterface: ClientLike + Sized {
  /// The blocking variant of [Self::lmpop].
  ///
  /// <https://redis.io/commands/blmpop/>
  fn blmpop<R, K>(
    &self,
    timeout: f64,
    keys: K,
    direction: LMoveDirection,
    count: Option<i64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::lists::blmpop(self, timeout, keys, direction, count)
        .await?
        .convert()
    }
  }

  /// BLPOP is a blocking list pop primitive. It is the blocking version of LPOP because it blocks the connection when
  /// there are no elements to pop from any of the given lists. An element is popped from the head of the first list
  /// that is non-empty, with the given keys being checked in the order that they are given.
  ///
  /// <https://redis.io/commands/blpop>
  fn blpop<R, K>(&self, keys: K, timeout: f64) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::lists::blpop(self, keys, timeout).await?.convert()
    }
  }

  /// BRPOP is a blocking list pop primitive. It is the blocking version of RPOP because it blocks the connection when
  /// there are no elements to pop from any of the given lists. An element is popped from the tail of the first list
  /// that is non-empty, with the given keys being checked in the order that they are given.
  ///
  /// <https://redis.io/commands/brpop>
  fn brpop<R, K>(&self, keys: K, timeout: f64) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::lists::brpop(self, keys, timeout).await?.convert()
    }
  }

  /// The blocking equivalent of [Self::rpoplpush].
  ///
  /// <https://redis.io/commands/brpoplpush>
  fn brpoplpush<R, S, D>(&self, source: S, destination: D, timeout: f64) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Key> + Send,
    D: Into<Key> + Send,
  {
    async move {
      into!(source, destination);
      commands::lists::brpoplpush(self, source, destination, timeout)
        .await?
        .convert()
    }
  }

  /// The blocking equivalent of [Self::lmove].
  ///
  /// <https://redis.io/commands/blmove>
  fn blmove<R, S, D>(
    &self,
    source: S,
    destination: D,
    source_direction: LMoveDirection,
    destination_direction: LMoveDirection,
    timeout: f64,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Key> + Send,
    D: Into<Key> + Send,
  {
    async move {
      into!(source, destination);
      commands::lists::blmove(
        self,
        source,
        destination,
        source_direction,
        destination_direction,
        timeout,
      )
      .await?
      .convert()
    }
  }

  /// Pops one or more elements from the first non-empty list key from the list of provided key names.
  ///
  /// <https://redis.io/commands/lmpop/>
  fn lmpop<R, K>(
    &self,
    keys: K,
    direction: LMoveDirection,
    count: Option<i64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
  {
    async move {
      into!(keys);
      commands::lists::lmpop(self, keys, direction, count).await?.convert()
    }
  }

  /// Returns the element at index in the list stored at key.
  ///
  /// <https://redis.io/commands/lindex>
  fn lindex<R, K>(&self, key: K, index: i64) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::lists::lindex(self, key, index).await?.convert()
    }
  }

  /// Inserts element in the list stored at key either before or after the reference value `pivot`.
  ///
  /// <https://redis.io/commands/linsert>
  fn linsert<R, K, P, V>(
    &self,
    key: K,
    location: ListLocation,
    pivot: P,
    element: V,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: TryInto<Value> + Send,
    P::Error: Into<Error> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(pivot, element);
      commands::lists::linsert(self, key, location, pivot, element)
        .await?
        .convert()
    }
  }

  /// Returns the length of the list stored at key.
  ///
  /// <https://redis.io/commands/llen>
  fn llen<R, K>(&self, key: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::lists::llen(self, key).await?.convert()
    }
  }

  /// Removes and returns the first elements of the list stored at key.
  ///
  /// <https://redis.io/commands/lpop>
  fn lpop<R, K>(&self, key: K, count: Option<usize>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::lists::lpop(self, key, count).await?.convert()
    }
  }

  /// The command returns the index of matching elements inside a Redis list.
  ///
  /// <https://redis.io/commands/lpos>
  fn lpos<R, K, V>(
    &self,
    key: K,
    element: V,
    rank: Option<i64>,
    count: Option<i64>,
    maxlen: Option<i64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(element);
      commands::lists::lpos(self, key, element, rank, count, maxlen)
        .await?
        .convert()
    }
  }

  /// Insert all the specified values at the head of the list stored at `key`.
  ///
  /// <https://redis.io/commands/lpush>
  fn lpush<R, K, V>(&self, key: K, elements: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(elements);
      commands::lists::lpush(self, key, elements).await?.convert()
    }
  }

  /// Inserts specified values at the head of the list stored at `key`, only if `key` already exists and holds a list.
  ///
  /// <https://redis.io/commands/lpushx>
  fn lpushx<R, K, V>(&self, key: K, elements: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(elements);
      commands::lists::lpushx(self, key, elements).await?.convert()
    }
  }

  /// Returns the specified elements of the list stored at `key`.
  ///
  /// <https://redis.io/commands/lrange>
  fn lrange<R, K>(&self, key: K, start: i64, stop: i64) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::lists::lrange(self, key, start, stop).await?.convert()
    }
  }

  /// Removes the first `count` occurrences of elements equal to `element` from the list stored at `key`.
  ///
  /// <https://redis.io/commands/lrem>
  fn lrem<R, K, V>(&self, key: K, count: i64, element: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(element);
      commands::lists::lrem(self, key, count, element).await?.convert()
    }
  }

  /// Sets the list element at `index` to `element`.
  ///
  /// <https://redis.io/commands/lset>
  fn lset<R, K, V>(&self, key: K, index: i64, element: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(element);
      commands::lists::lset(self, key, index, element).await?.convert()
    }
  }

  /// Trim an existing list so that it will contain only the specified range of elements specified.
  ///
  /// <https://redis.io/commands/ltrim>
  fn ltrim<R, K>(&self, key: K, start: i64, stop: i64) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::lists::ltrim(self, key, start, stop).await?.convert()
    }
  }

  /// Removes and returns the last elements of the list stored at `key`.
  ///
  /// <https://redis.io/commands/rpop>
  fn rpop<R, K>(&self, key: K, count: Option<usize>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::lists::rpop(self, key, count).await?.convert()
    }
  }

  /// Atomically returns and removes the last element (tail) of the list stored at `source`, and pushes the element at
  /// the first element (head) of the list stored at `destination`.
  ///
  /// <https://redis.io/commands/rpoplpush>
  fn rpoplpush<R, S, D>(&self, source: S, dest: D) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Key> + Send,
    D: Into<Key> + Send,
  {
    async move {
      into!(source, dest);
      commands::lists::rpoplpush(self, source, dest).await?.convert()
    }
  }

  /// Atomically returns and removes the first/last element (head/tail depending on the source direction argument) of
  /// the list stored at `source`, and pushes the element at the first/last element (head/tail depending on the
  /// destination direction argument) of the list stored at `destination`.
  ///
  /// <https://redis.io/commands/lmove>
  fn lmove<R, S, D>(
    &self,
    source: S,
    dest: D,
    source_direction: LMoveDirection,
    dest_direction: LMoveDirection,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Key> + Send,
    D: Into<Key> + Send,
  {
    async move {
      into!(source, dest);
      commands::lists::lmove(self, source, dest, source_direction, dest_direction)
        .await?
        .convert()
    }
  }

  /// Insert all the specified values at the tail of the list stored at `key`.
  ///
  /// <https://redis.io/commands/rpush>
  fn rpush<R, K, V>(&self, key: K, elements: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(elements);
      commands::lists::rpush(self, key, elements).await?.convert()
    }
  }

  /// Inserts specified values at the tail of the list stored at `key`, only if key already exists and holds a list.
  ///
  /// <https://redis.io/commands/rpushx>
  fn rpushx<R, K, V>(&self, key: K, elements: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(elements);
      commands::lists::rpushx(self, key, elements).await?.convert()
    }
  }

  /// Returns or stores the elements contained in the list, set or sorted set at `key`.
  ///
  /// <https://redis.io/commands/sort/>
  fn sort<R, K, S>(
    &self,
    key: K,
    by: Option<Str>,
    limit: Option<Limit>,
    get: S,
    order: Option<SortOrder>,
    alpha: bool,
    store: Option<Key>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(key, get);
      commands::lists::sort(self, key, by, limit, get, order, alpha, store)
        .await?
        .convert()
    }
  }

  /// Read-only variant of the SORT command. It is exactly like the original SORT but refuses the STORE option and can
  /// safely be used in read-only replicas.
  ///
  /// <https://redis.io/commands/sort_ro/>
  fn sort_ro<R, K, S>(
    &self,
    key: K,
    by: Option<Str>,
    limit: Option<Limit>,
    get: S,
    order: Option<SortOrder>,
    alpha: bool,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(key, get);
      commands::lists::sort_ro(self, key, by, limit, get, order, alpha)
        .await?
        .convert()
    }
  }
}
