use crate::{
  clients::Client,
  error::{Error, ErrorKind},
  interfaces::{ClientLike, MetricsInterface},
  prelude::{Config, ConnectionConfig, FredResult, PerformanceConfig, ReconnectPolicy, Server},
  runtime,
  runtime::{AtomicBool, AtomicUsize, JoinHandle, Mutex, RefCount, RefSwapOption},
  types::{
    config::DynamicPoolConfig,
    stats::{ClientUsage, PoolStats},
    ClientState,
    ConnectHandle,
  },
  utils,
};
use futures::future::join_all;
use std::{cmp, collections::HashMap, iter::repeat_with, ops::DerefMut, time::Duration};

#[cfg(all(feature = "dynamic-pool", feature = "glommio"))]
compile_error!("The `DynamicPool` interface is not currently supported with the Glommio runtime.");

/// An iterator that iterates over a dynamic pool, starting with the fixed minimum set of clients.
#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
pub struct DynamicPoolIterator {
  inner: RefCount<DynamicPoolInner>,
  index: usize,
}

impl Iterator for DynamicPoolIterator {
  type Item = Client;

  fn next(&mut self) -> Option<Self::Item> {
    let mut offset = self.index;
    self.index += 1;

    if offset < self.inner.fixed.len() {
      Some(self.inner.fixed[offset].clone())
    } else {
      offset = offset.saturating_sub(self.inner.fixed.len());
      self.inner.dynamic[offset].load().as_ref().map(|c| c.as_ref().clone())
    }
  }
}

#[cfg(feature = "dynamic-pool")]
struct DynamicPoolInner {
  config:           DynamicPoolConfig,
  fixed:            Vec<Client>,
  dynamic:          Vec<RefSwapOption<Client>>,
  dynamic_len:      AtomicUsize,
  counter:          AtomicUsize,
  task:             Mutex<Option<JoinHandle<()>>>,
  prefer_connected: AtomicBool,
}

#[cfg(feature = "dynamic-pool")]
impl Drop for DynamicPoolInner {
  fn drop(&mut self) {
    if let Some(task) = self.task.lock().take() {
      task.abort();
    }
    let clients: Vec<_> = self.fixed.drain(..).collect();
    let dynamic: Vec<_> = self
      .dynamic
      .drain(..)
      .filter_map(|opt| opt.load().as_ref().map(|c| c.as_ref().clone()))
      .collect();

    runtime::spawn(async move {
      let mut tasks = Vec::with_capacity(clients.len() + dynamic.len());
      for client in clients.iter() {
        tasks.push(client.quit());
      }
      for client in dynamic.iter() {
        tasks.push(client.quit());
      }
      let _ = join_all(tasks).await;
    });
  }
}

#[cfg(feature = "dynamic-pool")]
impl DynamicPoolInner {
  /// Read the number of clients that are not in a connected state.
  pub fn disconnected(&self) -> usize {
    let mut disconnected = self
      .fixed
      .iter()
      .fold(0, |acc, client| if client.is_connected() { acc } else { acc + 1 });
    for idx in 0 .. utils::read_atomic(&self.dynamic_len) {
      if let Some(client) = self.dynamic[idx].load().as_ref() {
        if !client.is_connected() {
          disconnected += 1;
        }
      } else {
        break;
      }
    }
    disconnected
  }

  pub fn size(&self) -> usize {
    self.fixed.len() + utils::read_atomic(&self.dynamic_len)
  }

  pub async fn reset(&self, notify: bool) {
    utils::set_atomic(&self.dynamic_len, 0);
    let clients: Vec<_> = self
      .dynamic
      .iter()
      .filter_map(|c| c.swap(None).map(|c| c.as_ref().clone()))
      .collect();

    if notify {
      self.config.scale.on_removed(clients).await;
    } else {
      join_all(clients.iter().map(|c| c.quit())).await;
    }
  }

  pub fn pool_stats(&self) -> PoolStats {
    let mut clients = HashMap::with_capacity(self.size());
    for client in self.fixed.iter() {
      clients.insert(client.inner.id.clone(), ClientUsage {
        network_latency: client.take_network_latency_metrics(),
        total_latency:   client.take_latency_metrics(),
        state:           client.state(),
      });
    }
    for idx in 0 .. utils::read_atomic(&self.dynamic_len) {
      if let Some(client) = self.dynamic[idx].load().as_ref() {
        clients.insert(client.inner.id.clone(), ClientUsage {
          network_latency: client.take_network_latency_metrics(),
          total_latency:   client.take_latency_metrics(),
          state:           client.state(),
        });
      } else {
        break;
      }
    }

    PoolStats {
      disconnected: self.disconnected(),
      clients,
    }
  }
}

/// A round-robin client pool that can dynamically scale.
///
/// ```rust
/// use fred::{
///   clients::DynamicPool,
///   prelude::*,
///   types::config::{DynamicPoolConfig, PoolScale, RemoveIdle},
/// };
/// use std::{sync::Arc, time::Duration};
///
/// async fn example() -> Result<(), Error> {
///   let config = Config::from_url("redis://localhost:6379")?;
///   let pool_config = DynamicPoolConfig {
///     min_clients:   2,
///     max_clients:   15,
///     max_idle_time: Duration::from_secs(60 * 5),
///     // use a scale policy that only removes idle connections
///     scale:         Arc::new(RemoveIdle),
///   };
///   let pool = Builder::from_config(config)
///     .set_pool_config(pool_config)
///     .build_dynamic_pool()?;
///
///   pool.init().await?;
///   pool.start_scale_task(Duration::from_secs(10));
///
///   // use `next()` to interact with individual clients
///   for idx in 0..100 {
///     let _: () = pool.next().incr_by(format!("foo-{idx}"), idx).await?;
///   }
///
///   // scale the pool manually, or use the `PoolScale` trait to check metrics and scale on an interval
///   pool.scale(1).await;
///   pool.scale(-1).await;
///
///   // reset the pool to its initial state
///   pool.reset().await;
///   // close all clients in the pool
///   pool.quit().await?;
///   Ok(())
/// }
/// ```
///
/// See the [DynamicPoolConfig](crate::types::config::DynamicPoolConfig) docs or the `dynamic_pool` example for more
/// info.
#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
#[derive(Clone)]
pub struct DynamicPool {
  inner: RefCount<DynamicPoolInner>,
}

impl DynamicPool {
  /// Create a new dynamic pool.
  ///
  /// See the [Builder](crate::types::Builder) docs for more info.
  pub fn new(
    config: Config,
    perf: Option<PerformanceConfig>,
    connection: Option<ConnectionConfig>,
    policy: Option<ReconnectPolicy>,
    pool_config: DynamicPoolConfig,
  ) -> Result<Self, Error> {
    if pool_config.min_clients == 0 {
      Err(Error::new(ErrorKind::Config, "Pool cannot be empty."))
    } else {
      let additional = pool_config.max_clients - pool_config.min_clients;
      let mut fixed = Vec::with_capacity(pool_config.min_clients);
      let dynamic: Vec<_> = repeat_with(|| RefSwapOption::new(None)).take(additional).collect();

      for _ in 0 .. pool_config.min_clients {
        fixed.push(Client::new(
          config.clone(),
          perf.clone(),
          connection.clone(),
          policy.clone(),
        ));
      }

      Ok(DynamicPool {
        inner: RefCount::new(DynamicPoolInner {
          config: pool_config,
          dynamic_len: AtomicUsize::new(0),
          counter: AtomicUsize::new(0),
          task: Mutex::new(None),
          prefer_connected: AtomicBool::new(false),
          fixed,
          dynamic,
        }),
      })
    }
  }

  /// Spawn a task that periodically removes idle connections and calls
  /// [PoolScale::scale](crate::types::config::PoolScale::scale) to determine how to scale the pool.
  ///
  /// Calling this multiple times will abort the previous task.
  pub fn start_scale_task(&self, interval: Duration) {
    let _self = self.clone();
    let task = runtime::spawn(async move {
      loop {
        runtime::sleep(interval).await;
        trace!("Removing idle connections and checking to scale pool...");
        _self.remove_idle().await;

        let stats = _self.inner.pool_stats();
        let amount = _self.inner.config.scale.scale(stats);
        let amount = _self.scale(amount).await;
        if amount != 0 {
          debug!("Scale dynamic pool by {} clients", amount);
        }
      }
    });

    if let Some(old) = self.inner.task.lock().deref_mut().replace(task) {
      old.abort();
    }
  }

  /// Stop the task that periodically removes idle connections and calls
  /// [PoolScale::scale](crate::types::config::PoolScale::scale).
  pub fn stop_scale_task(&self) {
    if let Some(task) = self.inner.task.lock().deref_mut().take() {
      task.abort();
    }
  }

  /// Iterate over clients in the pool, starting with the fixed set of minimum connections.
  pub fn clients(&self) -> impl Iterator<Item = Client> {
    DynamicPoolIterator {
      inner: self.inner.clone(),
      index: 0,
    }
  }

  /// Read the client that should run the next command.
  pub fn next(&self) -> Client {
    let prefer_connected = utils::read_bool_atomic(&self.inner.prefer_connected);
    let counter = utils::incr_atomic(&self.inner.counter);
    let fixed_len = self.inner.fixed.len();
    let dynamic_len = utils::read_atomic(&self.inner.dynamic_len);
    let total_len = fixed_len + dynamic_len;
    let mut offset = counter % total_len;

    if offset < fixed_len {
      // try the clients in the fixed pool, preferring a connected one if possible
      for i in 0 .. fixed_len {
        offset = (offset + i) % fixed_len;
        if prefer_connected && !self.inner.fixed[offset].is_connected() {
          // try another client from the fixed pool, wrapping around if needed
          continue;
        } else {
          break;
        }
      }

      self.inner.fixed[offset].clone()
    } else {
      // try to find a client from the dynamic pool, preferring a connected one if possible
      offset = offset.saturating_sub(fixed_len);
      for idx in 0 .. dynamic_len {
        let offset = (offset + idx) % dynamic_len;
        if let Some(client) = self.inner.dynamic[offset].load().as_ref() {
          if prefer_connected && !client.is_connected() {
            continue;
          } else {
            return client.as_ref().clone();
          }
        }
      }

      // fall back to the fixed pool, which cannot be empty
      self.inner.fixed[counter % fixed_len].clone()
    }
  }

  /// Add one client to the pool, if possible.
  ///
  /// If the client cannot connect on the first attempt the underlying error will be returned.
  ///
  /// This function will not call the `on_added` callback on the associated pool config.
  pub(crate) async fn add_client(&self) -> Result<Client, Error> {
    if self.size() >= self.inner.config.max_clients {
      return Err(Error::new(ErrorKind::Unknown, "Pool is full."));
    }

    let client = if let Some(client) = self.inner.fixed.first() {
      let mut config = client.client_config();
      config.fail_fast = true;
      let perf_config = client.perf_config();
      let connection_config = client.connection_config().clone();
      let policy = client.client_reconnect_policy();
      Client::new(config, Some(perf_config), Some(connection_config), policy)
    } else {
      return Err(Error::new(ErrorKind::Config, "Pool cannot be empty."));
    };
    #[cfg(feature = "dns")]
    if let Some(resolver) = self.inner.config.resolver.as_ref() {
      client.set_resolver(resolver.clone()).await;
    }
    client.init().await?;

    let client_ref = RefCount::new(client.clone());
    for client_opt in self.inner.dynamic.iter() {
      let swap_result = client_opt.compare_and_swap(&None::<RefCount<Client>>, Some(client_ref.clone()));
      if swap_result.is_none() {
        break;
      }
    }
    utils::incr_atomic(&self.inner.dynamic_len);
    Ok(client)
  }

  /// Add clients to the pool without length checks.
  pub(crate) async fn add_clients_unchecked(&self, amount: usize) -> usize {
    let tasks: Vec<_> = (0 .. amount).map(|_| self.add_client()).collect();
    let results: Vec<_> = join_all(tasks).await;
    let mut clients = Vec::with_capacity(results.len());
    let mut errors = Vec::new();

    for result in results.into_iter() {
      match result {
        Ok(client) => clients.push(client),
        Err(error) => errors.push(error),
      };
    }
    join_all(errors.into_iter().map(|error| async move {
      self.inner.config.scale.on_failure(error).await;
    }))
    .await;

    let amount = clients.len();
    self.inner.config.scale.on_added(clients).await;
    amount
  }

  /// Remove clients from the pool without length checks.
  pub(crate) async fn remove_clients_unchecked(&self, amount: usize) -> usize {
    let mut removed = Vec::with_capacity(amount);
    for idx in (0 .. utils::read_atomic(&self.inner.dynamic_len)).rev() {
      if let Some(client) = self.inner.dynamic[idx].swap(None) {
        removed.push(client.as_ref().clone());
        utils::decr_atomic(&self.inner.dynamic_len);
      }

      if removed.len() == amount {
        break;
      }
    }

    let amount = removed.len();
    self.inner.config.scale.on_removed(removed).await;
    amount
  }

  /// Scale the pool by the provided number of clients, returning the number of clients that were added or removed.
  ///
  /// This function waits for all clients to connect before returning. If a client cannot connect on the first attempt
  /// it will not be added to the pool.
  #[allow(clippy::comparison_chain)]
  pub async fn scale(&self, amount: i64) -> i64 {
    if amount < 0 {
      if self.size() == self.inner.fixed.len() {
        return 0;
      }

      let amount = cmp::min(
        utils::read_atomic(&self.inner.dynamic_len),
        amount.unsigned_abs() as usize,
      );
      if amount == 0 {
        return 0;
      }
      -(self.remove_clients_unchecked(amount).await as i64)
    } else if amount > 0 {
      if self.size() >= self.inner.config.max_clients {
        return 0;
      };

      let remaining = self.inner.config.max_clients - utils::read_atomic(&self.inner.dynamic_len);
      let amount = cmp::min(remaining, amount as usize);
      if amount == 0 {
        return 0;
      }
      self.add_clients_unchecked(amount).await as i64
    } else {
      0
    }
  }

  /// Check for idle connections and remove them from the pool.
  pub(crate) async fn remove_idle(&self) -> Vec<Client> {
    let mut to_remove = Vec::new();
    for (idx, client) in self.inner.dynamic.iter().enumerate() {
      if let Some(client) = client.load().as_ref() {
        if client.inner().last_command.load().elapsed() > self.inner.config.max_idle_time {
          to_remove.push(idx);
        }
      } else {
        break;
      }
    }

    let clients: Vec<_> = to_remove
      .into_iter()
      .filter_map(|idx| {
        if let Some(client) = self.inner.dynamic[idx].swap(None) {
          utils::decr_atomic(&self.inner.dynamic_len);
          Some(client.as_ref().clone())
        } else {
          None
        }
      })
      .collect();
    self.inner.config.scale.on_removed(clients.clone()).await;
    clients
  }

  /// Read the [DynamicPoolConfig](crate::types::config::DynamicPoolConfig) used to create the pool.
  pub fn pool_config(&self) -> &DynamicPoolConfig {
    &self.inner.config
  }

  /// Read the total size of the pool.
  pub fn size(&self) -> usize {
    self.inner.size()
  }

  /// Read the active connections used by the pool. This may contain duplicate server entries.
  pub fn active_connections(&self) -> Vec<Server> {
    let mut out = Vec::with_capacity(self.size());
    for client in self.inner.fixed.iter() {
      out.extend(client.active_connections());
    }
    for client_opt in self.inner.dynamic.iter() {
      if let Some(client) = client_opt.load().as_ref() {
        out.extend(client.active_connections());
      } else {
        break;
      }
    }
    out
  }

  /// Read the state of the least healthy connection.
  pub fn state(&self) -> ClientState {
    for client in self.inner.fixed.iter() {
      if client.state() != ClientState::Connected {
        return client.state();
      }
    }
    for client_opt in self.inner.dynamic.iter() {
      if let Some(client) = client_opt.load().as_ref() {
        if client.state() != ClientState::Connected {
          return client.state();
        }
      }
    }
    ClientState::Connected
  }

  /// Update the performance config on all the clients in the pool.
  pub fn update_perf_config(&self, config: PerformanceConfig) {
    for client in self.inner.fixed.iter() {
      client.update_perf_config(config.clone());
    }
    for client_opt in self.inner.dynamic.iter() {
      if let Some(client) = client_opt.load().as_ref() {
        client.update_perf_config(config.clone());
      }
    }
  }

  /// Set whether the client will prefer connected clients when calling [next](Self::next).
  pub fn prefer_connected(&self, val: bool) -> bool {
    utils::set_bool_atomic(&self.inner.prefer_connected, val)
  }

  /// Initialize a new routing and connection task for each client and wait for them to connect successfully.
  ///
  /// The returned [ConnectHandle](crate::types::ConnectHandle) refers to the task that drives the routing and
  /// connection layer for each client via [join](https://docs.rs/futures/latest/futures/macro.join.html). It will not finish until the max reconnection count is reached.
  ///
  /// ```rust
  /// use fred::prelude::*;
  ///
  /// #[tokio::main]
  /// async fn main() -> Result<(), Error> {
  ///   let pool = Builder::default_centralized().build_pool(5)?;
  ///   let connection_task = pool.init().await?;
  ///
  ///   // ...
  ///
  ///   pool.quit().await?;
  ///   connection_task.await?
  /// }
  /// ```
  pub async fn init(&self) -> FredResult<ConnectHandle> {
    self.inner.reset(true).await;
    #[cfg(feature = "dns")]
    if let Some(resolver) = self.inner.config.resolver.as_ref() {
      for client in self.inner.fixed.iter() {
        client.set_resolver(resolver.clone()).await;
      }
    }

    let mut rxs: Vec<_> = self
      .inner
      .fixed
      .iter()
      .map(|c| c.inner().notifications.connect.load().subscribe())
      .collect();

    let inner = self.inner.clone();
    let connect_task = runtime::spawn(async move {
      let tasks: Vec<_> = inner.fixed.iter().map(|c| c.connect()).collect();
      for result in join_all(tasks).await.into_iter() {
        result??;
      }
      debug!("Ending dynamic pool connection task");
      inner.reset(true).await;

      Ok::<(), Error>(())
    });
    let init_err = join_all(rxs.iter_mut().map(|rx| rx.recv()))
      .await
      .into_iter()
      .find_map(|result| match result {
        Ok(Err(e)) => Some(e),
        Err(e) => Some(e.into()),
        Ok(Ok(())) => None,
      });

    if let Some(err) = init_err {
      for client in self.inner.fixed.iter() {
        utils::reset_router_task(client.inner());
      }

      Err(err)
    } else {
      Ok(connect_task)
    }
  }

  /// Send `QUIT` and close all clients in the pool.
  ///
  /// This function also ends the task spawned by [start_scale_task](Self::start_scale_task).
  pub async fn quit(&self) -> FredResult<()> {
    if let Some(task) = self.inner.task.lock().take() {
      task.abort();
    }
    self.inner.reset(false).await;
    join_all(self.inner.fixed.iter().map(|c| c.quit())).await;

    Ok(())
  }

  /// Reset the pool to its initial state, dropping any dynamically created connections.
  ///
  /// Callers should use [quit](Self::quit) to close all connections.
  ///
  /// Any dropped connections will be sent to the [on_removed](crate::types::config::PoolScale::on_removed) callback.
  pub async fn reset(&self) {
    self.inner.reset(true).await;
  }
}
