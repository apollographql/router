use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{FromValue, Map, SentinelFailureKind, Value},
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use std::{convert::TryInto, net::IpAddr};

/// Functions that implement the [sentinel](https://redis.io/topics/sentinel#sentinel-commands) interface.
#[rm_send_if(feature = "glommio")]
pub trait SentinelInterface: ClientLike + Sized {
  /// Check if the current Sentinel configuration is able to reach the quorum needed to failover a master, and the
  /// majority needed to authorize the failover.
  fn ckquorum<R, N>(&self, name: N) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::ckquorum(self, name).await?.convert()
    }
  }

  /// Force Sentinel to rewrite its configuration on disk, including the current Sentinel state.
  fn flushconfig<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::sentinel::flushconfig(self).await?.convert() }
  }

  /// Force a failover as if the master was not reachable, and without asking for agreement to other Sentinels.
  fn failover<R, N>(&self, name: N) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::failover(self, name).await?.convert()
    }
  }

  /// Return the ip and port number of the master with that name.
  fn get_master_addr_by_name<R, N>(&self, name: N) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::get_master_addr_by_name(self, name).await?.convert()
    }
  }

  /// Return cached INFO output from masters and replicas.
  fn info_cache<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::sentinel::info_cache(self).await?.convert() }
  }

  /// Show the state and info of the specified master.
  fn master<R, N>(&self, name: N) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::master(self, name).await?.convert()
    }
  }

  /// Show a list of monitored masters and their state.
  fn masters<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::sentinel::masters(self).await?.convert() }
  }

  /// Start Sentinel's monitoring.
  ///
  /// <https://redis.io/topics/sentinel#reconfiguring-sentinel-at-runtime>
  fn monitor<R, N>(&self, name: N, ip: IpAddr, port: u16, quorum: u32) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::monitor(self, name, ip, port, quorum)
        .await?
        .convert()
    }
  }

  /// Return the ID of the Sentinel instance.
  fn myid<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::sentinel::myid(self).await?.convert() }
  }

  /// This command returns information about pending scripts.
  fn pending_scripts<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::sentinel::pending_scripts(self).await?.convert() }
  }

  /// Stop Sentinel's monitoring.
  ///
  /// <https://redis.io/topics/sentinel#reconfiguring-sentinel-at-runtime>
  fn remove<R, N>(&self, name: N) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::remove(self, name).await?.convert()
    }
  }

  /// Show a list of replicas for this master, and their state.
  fn replicas<R, N>(&self, name: N) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::replicas(self, name).await?.convert()
    }
  }

  /// Show a list of sentinel instances for this master, and their state.
  fn sentinels<R, N>(&self, name: N) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::sentinels(self, name).await?.convert()
    }
  }

  /// Set Sentinel's monitoring configuration.
  ///
  /// <https://redis.io/topics/sentinel#reconfiguring-sentinel-at-runtime>
  fn set<R, N, V>(&self, name: N, args: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    N: Into<Str> + Send,
    V: TryInto<Map> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(name);
      try_into!(args);
      commands::sentinel::set(self, name, args).await?.convert()
    }
  }

  /// This command simulates different Sentinel crash scenarios.
  fn simulate_failure<R>(&self, kind: SentinelFailureKind) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::sentinel::simulate_failure(self, kind).await?.convert() }
  }

  /// This command will reset all the masters with matching name.
  fn reset<R, P>(&self, pattern: P) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    P: Into<Str> + Send,
  {
    async move {
      into!(pattern);
      commands::sentinel::reset(self, pattern).await?.convert()
    }
  }

  /// Get the current value of a global Sentinel configuration parameter. The specified name may be a wildcard,
  /// similar to the Redis CONFIG GET command.
  fn config_get<R, K>(&self, name: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Str> + Send,
  {
    async move {
      into!(name);
      commands::sentinel::config_get(self, name).await?.convert()
    }
  }

  /// Set the value of a global Sentinel configuration parameter.
  fn config_set<R, K, V>(&self, name: K, value: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Str> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(name);
      try_into!(value);
      commands::sentinel::config_set(self, name, value).await?.convert()
    }
  }
}
