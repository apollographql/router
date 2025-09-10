use crate::{
  commands,
  interfaces::{ClientLike, FredResult},
  types::{FromValue, Key, MultipleKeys, MultipleStrings, SetOptions},
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use serde_json::Value;

/// The client commands in the [RedisJSON](https://redis.io/docs/data-types/json/) interface.
///
/// ## String Values
///
/// This interface uses [serde_json::Value](serde_json::Value) as the baseline type and will convert non-string values
/// to RESP bulk strings via [to_string](serde_json::to_string).
///
/// Some of the RedisJSON commands include the following notice in the documentation:
///
/// > To specify a string as an array value to append, wrap the quoted string with an additional set of single quotes.
/// > Example: '"silver"'.
///
/// The [serde_json::to_string](serde_json::to_string) functions are often the easiest way to do
/// this. The [json_quote](crate::json_quote) macro can also help.
///
/// For example:
///
/// ```rust
/// use fred::{json_quote, prelude::*};
/// use serde_json::json;
/// async fn example(client: &Client) -> Result<(), Error> {
///   let _: () = client.json_set("foo", "$", json!(["a", "b"]), None).await?;
///
///   // need to double quote string values in this context
///   let size: i64 = client
///     .json_arrappend("foo", Some("$"), vec![
///       json!("c").to_string(),
///       // or
///       serde_json::to_string(&json!("d"))?,
///       // or
///       json_quote!("e"),
///     ])
///     .await?;
///   assert_eq!(size, 5);
///   Ok(())
/// }
/// ```
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
#[rm_send_if(feature = "glommio")]
pub trait RedisJsonInterface: ClientLike + Sized {
  /// Append the json values into the array at path after the last element in it.
  ///
  /// <https://redis.io/commands/json.arrappend>
  fn json_arrappend<R, K, P, V>(&self, key: K, path: P, values: Vec<V>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      into!(key, path);
      let values = values.into_iter().map(|v| v.into()).collect();
      commands::redis_json::json_arrappend(self, key, path, values)
        .await?
        .convert()
    }
  }

  /// Search for the first occurrence of a JSON value in an array.
  ///
  /// <https://redis.io/commands/json.arrindex/>
  fn json_arrindex<R, K, P, V>(
    &self,
    key: K,
    path: P,
    value: V,
    start: Option<i64>,
    stop: Option<i64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      into!(key, path, value);
      commands::redis_json::json_arrindex(self, key, path, value, start, stop)
        .await?
        .convert()
    }
  }

  /// Insert the json values into the array at path before the index (shifts to the right).
  ///
  /// <https://redis.io/commands/json.arrinsert/>
  fn json_arrinsert<R, K, P, V>(
    &self,
    key: K,
    path: P,
    index: i64,
    values: Vec<V>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      into!(key, path);
      let values = values.into_iter().map(|v| v.into()).collect();
      commands::redis_json::json_arrinsert(self, key, path, index, values)
        .await?
        .convert()
    }
  }

  /// Report the length of the JSON array at path in key.
  ///
  /// <https://redis.io/commands/json.arrlen/>
  fn json_arrlen<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_arrlen(self, key, path).await?.convert()
    }
  }

  /// Remove and return an element from the index in the array
  ///
  /// <https://redis.io/commands/json.arrpop/>
  fn json_arrpop<R, K, P>(
    &self,
    key: K,
    path: Option<P>,
    index: Option<i64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_arrpop(self, key, path, index)
        .await?
        .convert()
    }
  }

  /// Trim an array so that it contains only the specified inclusive range of elements
  ///
  /// <https://redis.io/commands/json.arrtrim/>
  fn json_arrtrim<R, K, P>(
    &self,
    key: K,
    path: P,
    start: i64,
    stop: i64,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key, path);
      commands::redis_json::json_arrtrim(self, key, path, start, stop)
        .await?
        .convert()
    }
  }

  /// Clear container values (arrays/objects) and set numeric values to 0
  ///
  /// <https://redis.io/commands/json.clear/>
  fn json_clear<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_clear(self, key, path).await?.convert()
    }
  }

  /// Report a value's memory usage in bytes
  ///
  /// <https://redis.io/commands/json.debug-memory/>
  fn json_debug_memory<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_debug_memory(self, key, path)
        .await?
        .convert()
    }
  }

  /// Delete a value.
  ///
  /// <https://redis.io/commands/json.del/>
  fn json_del<R, K, P>(&self, key: K, path: P) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key, path);
      commands::redis_json::json_del(self, key, path).await?.convert()
    }
  }

  /// Return the value at path in JSON serialized form.
  ///
  /// <https://redis.io/commands/json.get/>
  fn json_get<R, K, I, N, S, P>(
    &self,
    key: K,
    indent: Option<I>,
    newline: Option<N>,
    space: Option<S>,
    paths: P,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    I: Into<Str> + Send,
    N: Into<Str> + Send,
    S: Into<Str> + Send,
    P: Into<MultipleStrings> + Send,
  {
    async move {
      into!(key, paths);
      let indent = indent.map(|v| v.into());
      let newline = newline.map(|v| v.into());
      let space = space.map(|v| v.into());
      commands::redis_json::json_get(self, key, indent, newline, space, paths)
        .await?
        .convert()
    }
  }

  /// Merge a given JSON value into matching paths.
  ///
  /// <https://redis.io/commands/json.merge/>
  fn json_merge<R, K, P, V>(&self, key: K, path: P, value: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      into!(key, path, value);
      commands::redis_json::json_merge(self, key, path, value)
        .await?
        .convert()
    }
  }

  /// Return the values at path from multiple key arguments.
  ///
  /// <https://redis.io/commands/json.mget/>
  fn json_mget<R, K, P>(&self, keys: K, path: P) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(keys, path);
      commands::redis_json::json_mget(self, keys, path).await?.convert()
    }
  }

  /// Set or update one or more JSON values according to the specified key-path-value triplets.
  ///
  /// <https://redis.io/commands/json.mset/>
  fn json_mset<R, K, P, V>(&self, values: Vec<(K, P, V)>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      let values = values
        .into_iter()
        .map(|(k, p, v)| (k.into(), p.into(), v.into()))
        .collect();
      commands::redis_json::json_mset(self, values).await?.convert()
    }
  }

  /// Increment the number value stored at path by number
  ///
  /// <https://redis.io/commands/json.numincrby/>
  fn json_numincrby<R, K, P, V>(&self, key: K, path: P, value: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      into!(key, path, value);
      commands::redis_json::json_numincrby(self, key, path, value)
        .await?
        .convert()
    }
  }

  /// Return the keys in the object that's referenced by path.
  ///
  /// <https://redis.io/commands/json.objkeys/>
  fn json_objkeys<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_objkeys(self, key, path).await?.convert()
    }
  }

  /// Report the number of keys in the JSON object at path in key.
  ///
  /// <https://redis.io/commands/json.objlen/>
  fn json_objlen<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_objlen(self, key, path).await?.convert()
    }
  }

  /// Return the JSON in key in Redis serialization protocol specification form.
  ///
  /// <https://redis.io/commands/json.resp/>
  fn json_resp<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_resp(self, key, path).await?.convert()
    }
  }

  /// Set the JSON value at path in key.
  ///
  /// <https://redis.io/commands/json.set/>
  fn json_set<R, K, P, V>(
    &self,
    key: K,
    path: P,
    value: V,
    options: Option<SetOptions>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      into!(key, path, value);
      commands::redis_json::json_set(self, key, path, value, options)
        .await?
        .convert()
    }
  }

  /// Append the json-string values to the string at path.
  ///
  /// <https://redis.io/commands/json.strappend/>
  fn json_strappend<R, K, P, V>(
    &self,
    key: K,
    path: Option<P>,
    value: V,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
    V: Into<Value> + Send,
  {
    async move {
      into!(key, value);
      let path = path.map(|p| p.into());
      commands::redis_json::json_strappend(self, key, path, value)
        .await?
        .convert()
    }
  }

  /// Report the length of the JSON String at path in key.
  ///
  /// <https://redis.io/commands/json.strlen/>
  fn json_strlen<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_strlen(self, key, path).await?.convert()
    }
  }

  /// Toggle a Boolean value stored at path.
  ///
  /// <https://redis.io/commands/json.toggle/>
  fn json_toggle<R, K, P>(&self, key: K, path: P) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key, path);
      commands::redis_json::json_toggle(self, key, path).await?.convert()
    }
  }

  /// Report the type of JSON value at path.
  ///
  /// <https://redis.io/commands/json.type/>
  fn json_type<R, K, P>(&self, key: K, path: Option<P>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key);
      let path = path.map(|p| p.into());
      commands::redis_json::json_type(self, key, path).await?.convert()
    }
  }
}
