use crate::{
  commands,
  interfaces::{ClientLike, FredResult},
  prelude::Error,
  types::{
    streams::{MultipleIDs, MultipleOrderedPairs, XCap, XPendingArgs, XReadResponse, XReadValue, XID},
    FromKey,
    FromValue,
    Key,
    MultipleKeys,
    MultipleStrings,
    Value,
  },
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use std::{convert::TryInto, hash::Hash};

/// Functions that implement the [streams](https://redis.io/commands#stream) interface.
///
/// **Note:** Several of the stream commands can return types with verbose type declarations. Additionally, certain
/// commands can be parsed differently in RESP2 and RESP3 modes. Functions such as [xread_map](Self::xread_map),
/// [xreadgroup_map](Self::xreadgroup_map), [xrange_values](Self::xrange_values), etc exist to make this easier for
/// callers. These functions apply an additional layer of parsing logic that can make declaring response types easier,
/// as well as automatically handling any differences between RESP2 and RESP3 return value types.
#[rm_send_if(feature = "glommio")]
pub trait StreamsInterface: ClientLike + Sized {
  /// This command returns the list of consumers that belong to the `groupname` consumer group of the stream stored at
  /// `key`.
  ///
  /// <https://redis.io/commands/xinfo-consumers>
  fn xinfo_consumers<R, K, S>(&self, key: K, groupname: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<Str> + Send,
  {
    async move {
      into!(key, groupname);
      commands::streams::xinfo_consumers(self, key, groupname)
        .await?
        .convert()
    }
  }

  /// This command returns the list of all consumers groups of the stream stored at `key`.
  ///
  /// <https://redis.io/commands/xinfo-groups>
  fn xinfo_groups<R, K>(&self, key: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::streams::xinfo_groups(self, key).await?.convert()
    }
  }

  /// This command returns information about the stream stored at `key`.
  ///
  /// <https://redis.io/commands/xinfo-stream>
  fn xinfo_stream<R, K>(&self, key: K, full: bool, count: Option<u64>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::streams::xinfo_stream(self, key, full, count).await?.convert()
    }
  }

  /// Appends the specified stream entry to the stream at the specified key. If the key does not exist, as a side
  /// effect of running this command the key is created with a stream value. The creation of stream's key can be
  /// disabled with the NOMKSTREAM option.
  ///
  /// <https://redis.io/commands/xadd>
  fn xadd<R, K, C, I, F>(
    &self,
    key: K,
    nomkstream: bool,
    cap: C,
    id: I,
    fields: F,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    I: Into<XID> + Send,
    F: TryInto<MultipleOrderedPairs> + Send,
    F::Error: Into<Error> + Send,
    C: TryInto<XCap> + Send,
    C::Error: Into<Error> + Send,
  {
    async move {
      into!(key, id);
      try_into!(fields, cap);
      commands::streams::xadd(self, key, nomkstream, cap, id, fields)
        .await?
        .convert()
    }
  }

  /// Trims the stream by evicting older entries (entries with lower IDs) if needed.
  ///
  /// <https://redis.io/commands/xtrim>
  fn xtrim<R, K, C>(&self, key: K, cap: C) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    C: TryInto<XCap> + Send,
    C::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(cap);
      commands::streams::xtrim(self, key, cap).await?.convert()
    }
  }

  /// Removes the specified entries from a stream, and returns the number of entries deleted.
  ///
  /// <https://redis.io/commands/xdel>
  fn xdel<R, K, S>(&self, key: K, ids: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(key, ids);
      commands::streams::xdel(self, key, ids).await?.convert()
    }
  }

  /// Return the stream entries matching the provided range of IDs, automatically converting to a less verbose type
  /// definition.
  ///
  /// <https://redis.io/commands/xrange>
  fn xrange_values<Ri, Rk, Rv, K, S, E>(
    &self,
    key: K,
    start: S,
    end: E,
    count: Option<u64>,
  ) -> impl Future<Output = FredResult<Vec<XReadValue<Ri, Rk, Rv>>>> + Send
  where
    Ri: FromValue,
    Rk: FromKey + Hash + Eq,
    Rv: FromValue,
    K: Into<Key> + Send,
    S: TryInto<Value> + Send,
    S::Error: Into<Error> + Send,
    E: TryInto<Value> + Send,
    E::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(start, end);
      commands::streams::xrange(self, key, start, end, count)
        .await?
        .into_xread_value()
    }
  }

  /// The command returns the stream entries matching a given range of IDs. The range is specified by a minimum
  /// and maximum ID. All the entries having an ID between the two specified or exactly one of the two IDs specified
  /// (closed interval) are returned.
  ///
  /// <https://redis.io/commands/xrange>
  ///
  /// **See [xrange_values](Self::xrange_values) for a variation of this function that may be more useful.**
  fn xrange<R, K, S, E>(
    &self,
    key: K,
    start: S,
    end: E,
    count: Option<u64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: TryInto<Value> + Send,
    S::Error: Into<Error> + Send,
    E: TryInto<Value> + Send,
    E::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(start, end);
      commands::streams::xrange(self, key, start, end, count).await?.convert()
    }
  }

  /// Similar to `XRANGE`, but with the results returned in reverse order. The results will be automatically converted
  /// to a less verbose type definition.
  ///
  /// <https://redis.io/commands/xrevrange>
  fn xrevrange_values<Ri, Rk, Rv, K, E, S>(
    &self,
    key: K,
    end: E,
    start: S,
    count: Option<u64>,
  ) -> impl Future<Output = FredResult<Vec<XReadValue<Ri, Rk, Rv>>>> + Send
  where
    Ri: FromValue,
    Rk: FromKey + Hash + Eq,
    Rv: FromValue,
    K: Into<Key> + Send,
    S: TryInto<Value> + Send,
    S::Error: Into<Error> + Send,
    E: TryInto<Value> + Send,
    E::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(start, end);
      commands::streams::xrevrange(self, key, end, start, count)
        .await?
        .into_xread_value()
    }
  }

  /// Similar to `XRANGE`, but with the results returned in reverse order.
  ///
  /// <https://redis.io/commands/xrevrange>
  ///
  /// **See the [xrevrange_values](Self::xrevrange_values) for a variation of this function that may be more useful.**
  fn xrevrange<R, K, S, E>(
    &self,
    key: K,
    end: E,
    start: S,
    count: Option<u64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: TryInto<Value> + Send,
    S::Error: Into<Error> + Send,
    E: TryInto<Value> + Send,
    E::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(start, end);
      commands::streams::xrevrange(self, key, end, start, count)
        .await?
        .convert()
    }
  }

  /// Returns the number of entries inside a stream.
  ///
  /// <https://redis.io/commands/xlen>
  fn xlen<R, K>(&self, key: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::streams::xlen(self, key).await?.convert()
    }
  }

  /// Read data from one or multiple streams, only returning entries with an ID greater than the last received ID
  /// reported by the caller.
  ///
  /// <https://redis.io/commands/xread>
  ///
  /// The `XREAD` and `XREADGROUP` commands return values that can be interpreted differently in RESP2 and RESP3 mode.
  /// In many cases it is also easier to operate on the return values of these functions as a `HashMap`, but
  /// manually declaring this type can be very verbose. This function will automatically convert the response to the
  /// [most common](crate::types::streams::XReadResponse) map representation while also handling the encoding
  /// differences between RESP2 and RESP3.
  ///
  /// ```rust no_run
  /// # use fred::{prelude::*, types::streams::XReadResponse};
  /// async fn example(client: Client) -> Result<(), Error> {
  ///   // borrowed from the tests. XREAD and XREADGROUP are very similar.
  ///   let result: XReadResponse<String, String, String, usize> = client  
  ///     .xreadgroup_map("group1", "consumer1", None, None, false, "foo", ">")
  ///     .await?;
  ///   println!("Result: {:?}", result);    
  ///   // Result: {"foo": [("1646240801081-0", {"count": 0}), ("1646240801082-0", {"count": 1}), ("1646240801082-1", {"count": 2})]}
  ///
  ///   assert_eq!(result.len(), 1);
  ///   for (idx, (id, record)) in result.get("foo").unwrap().into_iter().enumerate() {
  ///     let value = record.get("count").expect("Failed to read count");
  ///     assert_eq!(idx, *value);
  ///   }
  ///
  ///   Ok(())
  /// }
  /// ```
  // The underlying issue here isn't so much a semantic difference between RESP2 and RESP3, but rather an assumption
  // that went into the logic behind the `FromValue` trait.
  //
  // In all other Redis commands that return "maps" in RESP2 (or responses that should be interpreted as maps) a map
  // is encoded as an array with an even number of elements representing `(key, value)` pairs.
  //
  // As a result the `FromRedis` implementation for `HashMap`, `BTreeMap`, etc, took a dependency on this behavior. For example: https://redis.io/commands/hgetall#return-value
  //
  // ```
  // 127.0.0.1:6379> hset foo bar 0
  // (integer) 1
  // 127.0.0.1:6379> hset foo baz 1
  // (integer) 1
  // 127.0.0.1:6379> hgetall foo
  // 1) "bar"
  // 2) "0"
  // 3) "baz"
  // 4) "1"
  // // now switch to RESP3 which has a specific type for maps on the wire
  // 127.0.0.1:6379> hello 3
  // ...
  // 127.0.0.1:6379> hgetall foo
  // 1# "bar" => "0"
  // 2# "baz" => "1"
  // ```
  //
  // However, with XREAD/XREADGROUP there's an extra array wrapper in RESP2 around both the "outer" map and "inner"
  // map(s):
  //
  // ```
  // // RESP3
  // 127.0.0.1:6379> xread count 2 streams foo bar 1643479648480-0 1643479834990-0
  // 1# "foo" => 1) 1) "1643479650336-0"
  //       2) 1) "count"
  //          2) "3"
  // 2# "bar" => 1) 1) "1643479837746-0"
  //       2) 1) "count"
  //          2) "5"
  //    2) 1) "1643479925582-0"
  //       2) 1) "count"
  //          2) "6"
  //
  // // RESP2
  // 127.0.0.1:6379> xread count 2 streams foo bar 1643479648480-0 1643479834990-0
  // 1) 1) "foo"
  //    2) 1) 1) "1643479650336-0"
  //          2) 1) "count"
  //             2) "3"
  // 2) 1) "bar"
  //    2) 1) 1) "1643479837746-0"
  //          2) 1) "count"
  //             2) "5"
  //       2) 1) "1643479925582-0"
  //          2) 1) "count"
  //             2) "6"
  // ```
  //
  // If it helps, the underlying functions that do the RESP2 vs RESP3 conversion are public for callers as well, so
  // one could use a `BTreeMap` instead of a `HashMap` like so:
  //
  // ```
  // let value: BTreeMap<String, Vec<(String, BTreeMap<String, usize>)>> = client
  //   .xread::<Value, _, _>(None, None, "foo", "0")
  //   .await?
  //   .flatten_array_values(2)
  //   .convert()?;
  // ```
  fn xread_map<Rk1, Rk2, Rk3, Rv, K, I>(
    &self,
    count: Option<u64>,
    block: Option<u64>,
    keys: K,
    ids: I,
  ) -> impl Future<Output = FredResult<XReadResponse<Rk1, Rk2, Rk3, Rv>>> + Send
  where
    Rk1: FromKey + Hash + Eq,
    Rk2: FromValue,
    Rk3: FromKey + Hash + Eq,
    Rv: FromValue,
    K: Into<MultipleKeys> + Send,
    I: Into<MultipleIDs> + Send,
  {
    async move {
      into!(keys, ids);
      commands::streams::xread(self, count, block, keys, ids)
        .await?
        .into_xread_response()
    }
  }

  /// Read data from one or multiple streams, only returning entries with an ID greater than the last received ID
  /// reported by the caller.
  ///
  /// <https://redis.io/commands/xread>
  ///
  /// **See [xread_map](Self::xread_map) for more information on a variation of this function that might be more
  /// useful.**
  fn xread<R, K, I>(
    &self,
    count: Option<u64>,
    block: Option<u64>,
    keys: K,
    ids: I,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
    I: Into<MultipleIDs> + Send,
  {
    async move {
      into!(keys, ids);
      commands::streams::xread(self, count, block, keys, ids).await?.convert()
    }
  }

  /// This command creates a new consumer group uniquely identified by `groupname` for the stream stored at `key`.
  ///
  /// <https://redis.io/commands/xgroup-create>
  fn xgroup_create<R, K, S, I>(
    &self,
    key: K,
    groupname: S,
    id: I,
    mkstream: bool,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<Str> + Send,
    I: Into<XID> + Send,
  {
    async move {
      into!(key, groupname, id);
      commands::streams::xgroup_create(self, key, groupname, id, mkstream)
        .await?
        .convert()
    }
  }

  /// Create a consumer named `consumername` in the consumer group `groupname` of the stream that's stored at `key`.
  ///
  /// <https://redis.io/commands/xgroup-createconsumer>
  fn xgroup_createconsumer<R, K, G, C>(
    &self,
    key: K,
    groupname: G,
    consumername: C,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
  {
    async move {
      into!(key, groupname, consumername);
      commands::streams::xgroup_createconsumer(self, key, groupname, consumername)
        .await?
        .convert()
    }
  }

  /// Delete a consumer named `consumername` in the consumer group `groupname` of the stream that's stored at `key`.
  ///
  /// <https://redis.io/commands/xgroup-delconsumer>
  fn xgroup_delconsumer<R, K, G, C>(
    &self,
    key: K,
    groupname: G,
    consumername: C,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
  {
    async move {
      into!(key, groupname, consumername);
      commands::streams::xgroup_delconsumer(self, key, groupname, consumername)
        .await?
        .convert()
    }
  }

  /// Completely destroy a consumer group.
  ///
  /// <https://redis.io/commands/xgroup-destroy>
  fn xgroup_destroy<R, K, S>(&self, key: K, groupname: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<Str> + Send,
  {
    async move {
      into!(key, groupname);
      commands::streams::xgroup_destroy(self, key, groupname).await?.convert()
    }
  }

  /// Set the last delivered ID for a consumer group.
  ///
  /// <https://redis.io/commands/xgroup-setid>
  fn xgroup_setid<R, K, S, I>(&self, key: K, groupname: S, id: I) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<Str> + Send,
    I: Into<XID> + Send,
  {
    async move {
      into!(key, groupname, id);
      commands::streams::xgroup_setid(self, key, groupname, id)
        .await?
        .convert()
    }
  }

  /// A special version of the `XREAD` command with support for consumer groups.
  ///
  /// Declaring proper type declarations for this command can be complicated due to the complex nature of the response
  /// values and the differences between RESP2 and RESP3. See the [xread](Self::xread) documentation for more
  /// information.
  ///
  /// <https://redis.io/commands/xreadgroup>
  ///
  /// The `XREAD` and `XREADGROUP` commands return values that can be interpreted differently in RESP2 and RESP3 mode.
  /// In many cases it is also easier to operate on the return values of these functions as a `HashMap`, but
  /// manually declaring this type can be very verbose. This function will automatically convert the response to the
  /// [most common](crate::types::streams::XReadResponse) map representation while also handling the encoding
  /// differences between RESP2 and RESP3.
  ///
  /// See the [xread_map](Self::xread_map) documentation for more information.
  // See the `xread_map` source docs for more information.
  fn xreadgroup_map<Rk1, Rk2, Rk3, Rv, G, C, K, I>(
    &self,
    group: G,
    consumer: C,
    count: Option<u64>,
    block: Option<u64>,
    noack: bool,
    keys: K,
    ids: I,
  ) -> impl Future<Output = FredResult<XReadResponse<Rk1, Rk2, Rk3, Rv>>> + Send
  where
    Rk1: FromKey + Hash + Eq,
    Rk2: FromValue,
    Rk3: FromKey + Hash + Eq,
    Rv: FromValue,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
    K: Into<MultipleKeys> + Send,
    I: Into<MultipleIDs> + Send,
  {
    async move {
      into!(group, consumer, keys, ids);
      commands::streams::xreadgroup(self, group, consumer, count, block, noack, keys, ids)
        .await?
        .into_xread_response()
    }
  }

  /// A special version of the `XREAD` command with support for consumer groups.
  ///
  /// Declaring proper type declarations for this command can be complicated due to the complex nature of the response
  /// values and the differences between RESP2 and RESP3. See the [xread](Self::xread) documentation for more
  /// information.
  ///
  /// <https://redis.io/commands/xreadgroup>
  ///
  /// **See [xreadgroup_map](Self::xreadgroup_map) for a variation of this function that might be more useful.**
  fn xreadgroup<R, G, C, K, I>(
    &self,
    group: G,
    consumer: C,
    count: Option<u64>,
    block: Option<u64>,
    noack: bool,
    keys: K,
    ids: I,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
    K: Into<MultipleKeys> + Send,
    I: Into<MultipleIDs> + Send,
  {
    async move {
      into!(group, consumer, keys, ids);
      commands::streams::xreadgroup(self, group, consumer, count, block, noack, keys, ids)
        .await?
        .convert()
    }
  }

  /// Remove one or more messages from the Pending Entries List (PEL) of a stream consumer group.
  ///
  /// <https://redis.io/commands/xack>
  fn xack<R, K, G, I>(&self, key: K, group: G, ids: I) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    I: Into<MultipleIDs> + Send,
  {
    async move {
      into!(key, group, ids);
      commands::streams::xack(self, key, group, ids).await?.convert()
    }
  }

  /// A variation of [xclaim](Self::xclaim) with a less verbose return type.
  fn xclaim_values<Ri, Rk, Rv, K, G, C, I>(
    &self,
    key: K,
    group: G,
    consumer: C,
    min_idle_time: u64,
    ids: I,
    idle: Option<u64>,
    time: Option<u64>,
    retry_count: Option<u64>,
    force: bool,
    justid: bool,
  ) -> impl Future<Output = FredResult<Vec<XReadValue<Ri, Rk, Rv>>>> + Send
  where
    Ri: FromValue,
    Rk: FromKey + Hash + Eq,
    Rv: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
    I: Into<MultipleIDs> + Send,
  {
    async move {
      into!(key, group, consumer, ids);
      commands::streams::xclaim(
        self,
        key,
        group,
        consumer,
        min_idle_time,
        ids,
        idle,
        time,
        retry_count,
        force,
        justid,
      )
      .await?
      .into_xread_value()
    }
  }

  /// In the context of a stream consumer group, this command changes the ownership of a pending message,
  /// so that the new owner is the consumer specified as the command argument.
  ///
  /// <https://redis.io/commands/xclaim>
  ///
  /// **See [xclaim_values](Self::xclaim_values) for a variation of this function that might be more useful.**
  fn xclaim<R, K, G, C, I>(
    &self,
    key: K,
    group: G,
    consumer: C,
    min_idle_time: u64,
    ids: I,
    idle: Option<u64>,
    time: Option<u64>,
    retry_count: Option<u64>,
    force: bool,
    justid: bool,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
    I: Into<MultipleIDs> + Send,
  {
    async move {
      into!(key, group, consumer, ids);
      commands::streams::xclaim(
        self,
        key,
        group,
        consumer,
        min_idle_time,
        ids,
        idle,
        time,
        retry_count,
        force,
        justid,
      )
      .await?
      .convert()
    }
  }

  /// This command transfers ownership of pending stream entries that match the specified criteria. It also converts
  /// the response type to a less verbose type declaration and handles potential differences between RESP2 and RESP3.
  ///
  /// <https://redis.io/commands/xautoclaim>
  // FIXME: this type declaration wont work for Redis v7. Probably need a new FF for this...
  fn xautoclaim_values<Ri, Rk, Rv, K, G, C, I>(
    &self,
    key: K,
    group: G,
    consumer: C,
    min_idle_time: u64,
    start: I,
    count: Option<u64>,
    justid: bool,
  ) -> impl Future<Output = FredResult<(String, Vec<XReadValue<Ri, Rk, Rv>>)>> + Send
  where
    Ri: FromValue,
    Rk: FromKey + Hash + Eq,
    Rv: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
    I: Into<XID> + Send,
  {
    async move {
      into!(key, group, consumer, start);
      commands::streams::xautoclaim(self, key, group, consumer, min_idle_time, start, count, justid)
        .await?
        .into_xautoclaim_values()
    }
  }

  /// This command transfers ownership of pending stream entries that match the specified criteria.
  ///
  /// <https://redis.io/commands/xautoclaim>
  ///
  /// **Note: See [xautoclaim_values](Self::xautoclaim_values) for a variation of this function that may be more
  /// useful.**
  fn xautoclaim<R, K, G, C, I>(
    &self,
    key: K,
    group: G,
    consumer: C,
    min_idle_time: u64,
    start: I,
    count: Option<u64>,
    justid: bool,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    C: Into<Str> + Send,
    I: Into<XID> + Send,
  {
    async move {
      into!(key, group, consumer, start);
      commands::streams::xautoclaim(self, key, group, consumer, min_idle_time, start, count, justid)
        .await?
        .convert()
    }
  }

  /// Inspect the list of pending messages in a consumer group.
  ///
  /// <https://redis.io/commands/xpending>
  fn xpending<R, K, G, A>(&self, key: K, group: G, args: A) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    G: Into<Str> + Send,
    A: Into<XPendingArgs> + Send,
  {
    async move {
      into!(key, group, args);
      commands::streams::xpending(self, key, group, args).await?.convert()
    }
  }
}
