use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{
    scripts::{FnPolicy, ScriptDebugFlag},
    FromValue,
    MultipleKeys,
    MultipleStrings,
    MultipleValues,
  },
};
use bytes::Bytes;
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use std::convert::TryInto;

/// Functions that implement the [lua](https://redis.io/commands#lua) interface.
#[rm_send_if(feature = "glommio")]
pub trait LuaInterface: ClientLike + Sized {
  /// Load a script into the scripts cache, without executing it. After the specified command is loaded into the
  /// script cache it will be callable using EVALSHA with the correct SHA1 digest of the script.
  ///
  /// Returns the SHA-1 hash of the script.
  ///
  /// <https://redis.io/commands/script-load>
  fn script_load<R, S>(&self, script: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(script);
      commands::lua::script_load(self, script).await?.convert()
    }
  }

  /// A clustered variant of [script_load](Self::script_load) that loads the script on all primary nodes in a cluster.
  ///
  /// Returns the SHA-1 hash of the script.
  #[cfg(feature = "sha-1")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sha-1")))]
  fn script_load_cluster<R, S>(&self, script: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(script);
      commands::lua::script_load_cluster(self, script).await?.convert()
    }
  }

  /// Kills the currently executing Lua script, assuming no write operation was yet performed by the script.
  ///
  /// <https://redis.io/commands/script-kill>
  fn script_kill(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::lua::script_kill(self).await }
  }

  /// A clustered variant of the [script_kill](Self::script_kill) command that issues the command to all primary nodes
  /// in the cluster.
  fn script_kill_cluster(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::lua::script_kill_cluster(self).await }
  }

  /// Flush the Lua scripts cache.
  ///
  /// <https://redis.io/commands/script-flush>
  fn script_flush(&self, r#async: bool) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::lua::script_flush(self, r#async).await }
  }

  /// A clustered variant of [script_flush](Self::script_flush) that flushes the script cache on all primary nodes in
  /// the cluster.
  fn script_flush_cluster(&self, r#async: bool) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::lua::script_flush_cluster(self, r#async).await }
  }

  /// Returns information about the existence of the scripts in the script cache.
  ///
  /// <https://redis.io/commands/script-exists>
  fn script_exists<R, H>(&self, hashes: H) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    H: Into<MultipleStrings> + Send,
  {
    async move {
      into!(hashes);
      commands::lua::script_exists(self, hashes).await?.convert()
    }
  }

  /// Set the debug mode for subsequent scripts executed with EVAL.
  ///
  /// <https://redis.io/commands/script-debug>
  fn script_debug(&self, flag: ScriptDebugFlag) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::lua::script_debug(self, flag).await }
  }

  /// Evaluates a script cached on the server side by its SHA1 digest.
  ///
  /// <https://redis.io/commands/evalsha>
  ///
  /// **Note: Use `None` to represent an empty set of keys or args.**
  fn evalsha<R, S, K, V>(&self, hash: S, keys: K, args: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(hash, keys);
      try_into!(args);
      commands::lua::evalsha(self, hash, keys, args).await?.convert()
    }
  }

  /// Evaluate a Lua script on the server.
  ///
  /// <https://redis.io/commands/eval>
  ///
  /// **Note: Use `None` to represent an empty set of keys or args.**
  fn eval<R, S, K, V>(&self, script: S, keys: K, args: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(script, keys);
      try_into!(args);
      commands::lua::eval(self, script, keys, args).await?.convert()
    }
  }
}

/// Functions that implement the [function](https://redis.io/docs/manual/programmability/functions-intro/) interface.
#[rm_send_if(feature = "glommio")]
pub trait FunctionInterface: ClientLike + Sized {
  /// Invoke a function.
  ///
  /// <https://redis.io/commands/fcall/>
  fn fcall<R, F, K, V>(&self, func: F, keys: K, args: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    F: Into<Str> + Send,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(func);
      try_into!(keys, args);
      commands::lua::fcall(self, func, keys, args).await?.convert()
    }
  }

  /// This is a read-only variant of the FCALL command that cannot execute commands that modify data.
  ///
  /// <https://redis.io/commands/fcall_ro/>
  fn fcall_ro<R, F, K, V>(&self, func: F, keys: K, args: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    F: Into<Str> + Send,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(func);
      try_into!(keys, args);
      commands::lua::fcall_ro(self, func, keys, args).await?.convert()
    }
  }

  /// Delete a library and all its functions.
  ///
  /// <https://redis.io/commands/function-delete/>
  fn function_delete<R, S>(&self, library_name: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(library_name);
      commands::lua::function_delete(self, library_name).await?.convert()
    }
  }

  /// Delete a library and all its functions from each cluster node concurrently.
  ///
  /// <https://redis.io/commands/function-delete/>
  fn function_delete_cluster<S>(&self, library_name: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<Str> + Send,
  {
    async move {
      into!(library_name);
      commands::lua::function_delete_cluster(self, library_name).await
    }
  }

  /// Return the serialized payload of loaded libraries.
  ///
  /// <https://redis.io/commands/function-dump/>
  fn function_dump<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::lua::function_dump(self).await?.convert() }
  }

  /// Deletes all the libraries.
  ///
  /// <https://redis.io/commands/function-flush/>
  fn function_flush<R>(&self, r#async: bool) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::lua::function_flush(self, r#async).await?.convert() }
  }

  /// Deletes all the libraries on all cluster nodes concurrently.
  ///
  /// <https://redis.io/commands/function-flush/>
  fn function_flush_cluster(&self, r#async: bool) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::lua::function_flush_cluster(self, r#async).await }
  }

  /// Kill a function that is currently executing.
  ///
  /// Note: This command runs on a backchannel connection to the server in order to take effect as quickly as
  /// possible.
  ///
  /// <https://redis.io/commands/function-kill/>
  fn function_kill<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::lua::function_kill(self).await?.convert() }
  }

  /// Return information about the functions and libraries.
  ///
  /// <https://redis.io/commands/function-list/>
  fn function_list<R, S>(&self, library_name: Option<S>, withcode: bool) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      let library_name = library_name.map(|l| l.into());
      commands::lua::function_list(self, library_name, withcode)
        .await?
        .convert()
    }
  }

  /// Load a library to Redis.
  ///
  /// <https://redis.io/commands/function-load/>
  fn function_load<R, S>(&self, replace: bool, code: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(code);
      commands::lua::function_load(self, replace, code).await?.convert()
    }
  }

  /// Load a library to Redis on all cluster nodes concurrently.
  ///
  /// <https://redis.io/commands/function-load/>
  fn function_load_cluster<R, S>(&self, replace: bool, code: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(code);
      commands::lua::function_load_cluster(self, replace, code)
        .await?
        .convert()
    }
  }

  /// Restore libraries from the serialized payload.
  ///
  /// <https://redis.io/commands/function-restore/>
  ///
  /// Note: Use `FnPolicy::default()` to use the default function restore policy (`"APPEND"`).
  fn function_restore<R, B, P>(&self, serialized: B, policy: P) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    B: Into<Bytes> + Send,
    P: TryInto<FnPolicy> + Send,
    P::Error: Into<Error> + Send,
  {
    async move {
      into!(serialized);
      try_into!(policy);
      commands::lua::function_restore(self, serialized, policy)
        .await?
        .convert()
    }
  }

  /// Restore libraries from the serialized payload on all cluster nodes concurrently.
  ///
  /// <https://redis.io/commands/function-restore/>
  ///
  /// Note: Use `FnPolicy::default()` to use the default function restore policy (`"APPEND"`).
  fn function_restore_cluster<B, P>(&self, serialized: B, policy: P) -> impl Future<Output = FredResult<()>> + Send
  where
    B: Into<Bytes> + Send,
    P: TryInto<FnPolicy> + Send,
    P::Error: Into<Error> + Send,
  {
    async move {
      into!(serialized);
      try_into!(policy);
      commands::lua::function_restore_cluster(self, serialized, policy).await
    }
  }

  /// Return information about the function that's currently running and information about the available execution
  /// engines.
  ///
  /// Note: This command runs on a backchannel connection to the server.
  ///
  /// <https://redis.io/commands/function-stats/>
  fn function_stats<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::lua::function_stats(self).await?.convert() }
  }
}
