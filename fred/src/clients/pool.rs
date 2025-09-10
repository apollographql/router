#[cfg(feature = "replicas")]
use crate::clients::Replicas;
#[cfg(feature = "dns")]
use crate::protocol::types::Resolve;
use crate::{
  clients::Client,
  error::{Error, ErrorKind},
  interfaces::*,
  modules::inner::ClientInner,
  runtime::{sleep, spawn, AtomicBool, AtomicUsize, RefCount},
  types::{
    config::{Config, ConnectionConfig, PerformanceConfig, ReconnectPolicy, Server},
    ConnectHandle,
  },
  utils,
};
use fred_macros::rm_send_if;
use futures::future::{join_all, try_join_all};
use std::{fmt, future::Future, time::Duration};

#[cfg(not(feature = "glommio"))]
pub use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

struct PoolInner {
  clients:          Vec<Client>,
  counter:          AtomicUsize,
  prefer_connected: AtomicBool,
}

/// A cheaply cloneable round-robin client pool.
///
/// ### Restrictions
///
/// The following interfaces are not implemented on `Pool`:
/// * [MetricsInterface](crate::interfaces::MetricsInterface)
/// * [PubsubInterface](crate::interfaces::PubsubInterface)
/// * [EventInterface](crate::interfaces::EventInterface)
/// * [ClientInterface](crate::interfaces::ClientInterface)
/// * [AuthInterface](crate::interfaces::AuthInterface)
///
/// In many cases, such as [publish](crate::interfaces::PubsubInterface::publish), callers can work around this by
/// adding a call to [next](Self::next), but in some scenarios this may not work. As a general rule, any commands
/// that change or depend on local connection state will not be implemented directly on `Pool`. Callers can use
/// [clients](Self::clients), [next](Self::next), or [last](Self::last) to operate on individual clients if needed.
#[derive(Clone)]
pub struct Pool {
  inner: RefCount<PoolInner>,
}

impl fmt::Debug for Pool {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    f.debug_struct("RedisPool")
      .field("size", &self.inner.clients.len())
      .field(
        "prefer_connected",
        &utils::read_bool_atomic(&self.inner.prefer_connected),
      )
      .finish()
  }
}

impl Pool {
  /// Create a new pool from an existing set of clients.
  pub fn from_clients(clients: Vec<Client>) -> Result<Self, Error> {
    if clients.is_empty() {
      Err(Error::new(ErrorKind::Config, "Pool cannot be empty."))
    } else {
      Ok(Pool {
        inner: RefCount::new(PoolInner {
          clients,
          counter: AtomicUsize::new(0),
          prefer_connected: AtomicBool::new(true),
        }),
      })
    }
  }

  /// Create a new pool without connecting to the server.
  ///
  /// See the [builder](crate::types::Builder) interface for more information.
  pub fn new(
    config: Config,
    perf: Option<PerformanceConfig>,
    connection: Option<ConnectionConfig>,
    policy: Option<ReconnectPolicy>,
    size: usize,
  ) -> Result<Self, Error> {
    if size == 0 {
      Err(Error::new(ErrorKind::Config, "Pool cannot be empty."))
    } else {
      let mut clients = Vec::with_capacity(size);
      for _ in 0 .. size {
        clients.push(Client::new(
          config.clone(),
          perf.clone(),
          connection.clone(),
          policy.clone(),
        ));
      }

      Ok(Pool {
        inner: RefCount::new(PoolInner {
          clients,
          counter: AtomicUsize::new(0),
          prefer_connected: AtomicBool::new(true),
        }),
      })
    }
  }

  /// Set whether the client will use [next_connected](Self::next_connected) or [next](Self::next) when routing
  /// commands among the pooled clients.
  pub fn prefer_connected(&self, val: bool) -> bool {
    utils::set_bool_atomic(&self.inner.prefer_connected, val)
  }

  /// Read the individual clients in the pool.
  pub fn clients(&self) -> &[Client] {
    &self.inner.clients
  }

  /// Connect each client to the server, returning the task driving each connection.
  ///
  /// Use the base [connect](Self::connect) function to return one handle that drives all connections via [join](https://docs.rs/futures/latest/futures/macro.join.html).
  pub fn connect_pool(&self) -> Vec<ConnectHandle> {
    self.inner.clients.iter().map(|c| c.connect()).collect()
  }

  /// Read the size of the pool.
  pub fn size(&self) -> usize {
    self.inner.clients.len()
  }

  /// Read the next connected client that should run the next command.
  pub fn next_connected(&self) -> &Client {
    let mut idx = utils::incr_atomic(&self.inner.counter) % self.inner.clients.len();

    for _ in 0 .. self.inner.clients.len() {
      let client = &self.inner.clients[idx];
      if client.is_connected() {
        return client;
      }
      idx = (idx + 1) % self.inner.clients.len();
    }

    &self.inner.clients[idx]
  }

  /// Read the client that should run the next command.
  pub fn next(&self) -> &Client {
    &self.inner.clients[utils::incr_atomic(&self.inner.counter) % self.inner.clients.len()]
  }

  /// Read the client that ran the last command.
  pub fn last(&self) -> &Client {
    &self.inner.clients[utils::read_atomic(&self.inner.counter) % self.inner.clients.len()]
  }

  /// Create a client that interacts with the replica nodes associated with the [next](Self::next) client.
  #[cfg(feature = "replicas")]
  #[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
  pub fn replicas(&self) -> Replicas<Client> {
    Replicas::from(self.inner())
  }
}

#[rm_send_if(feature = "glommio")]
impl ClientLike for Pool {
  #[doc(hidden)]
  fn inner(&self) -> &RefCount<ClientInner> {
    if utils::read_bool_atomic(&self.inner.prefer_connected) {
      &self.next_connected().inner
    } else {
      &self.next().inner
    }
  }

  /// Update the internal [PerformanceConfig](crate::types::config::PerformanceConfig) on each client in place with
  /// new values.
  fn update_perf_config(&self, config: PerformanceConfig) {
    for client in self.inner.clients.iter() {
      client.update_perf_config(config.clone());
    }
  }

  /// Read the set of active connections across all clients in the pool.
  ///
  /// This may contain duplicates when separate clients are connected to the same server.
  fn active_connections(&self) -> Vec<Server> {
    self.inner.clients.iter().flat_map(|c| c.active_connections()).collect()
  }

  /// Override the DNS resolution logic for all clients in the pool.
  #[cfg(feature = "dns")]
  #[cfg_attr(docsrs, doc(cfg(feature = "dns")))]
  #[allow(refining_impl_trait)]
  fn set_resolver(&self, resolver: RefCount<dyn Resolve>) -> impl Future + Send {
    async move {
      for client in self.inner.clients.iter() {
        client.set_resolver(resolver.clone()).await;
      }
    }
  }

  /// Connect each client to the server.
  ///
  /// This function returns a `JoinHandle` to a task that drives **all** connections via [join](https://docs.rs/futures/latest/futures/macro.join.html).
  ///
  /// See [connect_pool](crate::clients::Pool::connect_pool) for a variation of this function that separates the
  /// connection tasks.
  ///
  /// See [init](Self::init) for an alternative shorthand.
  fn connect(&self) -> ConnectHandle {
    let clients = self.inner.clients.clone();
    spawn(async move {
      let tasks: Vec<_> = clients.iter().map(|c| c.connect()).collect();
      for result in join_all(tasks).await.into_iter() {
        result??;
      }

      Ok::<(), Error>(())
    })
  }

  /// Force a reconnection to the server(s) for each client.
  ///
  /// When running against a cluster this function will also refresh the cached cluster routing table.
  fn force_reconnection(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move {
      try_join_all(self.inner.clients.iter().map(|c| c.force_reconnection())).await?;
      Ok(())
    }
  }

  /// Wait for all the clients to connect to the server.
  fn wait_for_connect(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move {
      try_join_all(self.inner.clients.iter().map(|c| c.wait_for_connect())).await?;
      Ok(())
    }
  }

  /// Initialize a new routing and connection task for each client and wait for them to connect successfully.
  ///
  /// The returned [ConnectHandle](crate::types::ConnectHandle) refers to the task that drives the routing and
  /// connection layer for each client via [join](https://docs.rs/futures/latest/futures/macro.join.html). It will not finish until the max reconnection count is reached.
  ///
  /// Callers can also use [connect](Self::connect) and [wait_for_connect](Self::wait_for_connect) separately if
  /// needed.
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
  fn init(&self) -> impl Future<Output = FredResult<ConnectHandle>> + Send {
    #[allow(unused_mut)]
    async move {
      let mut rxs: Vec<_> = self
        .inner
        .clients
        .iter()
        .map(|c| c.inner().notifications.connect.load().subscribe())
        .collect();

      let connect_task = self.connect();
      let init_err = futures::future::join_all(rxs.iter_mut().map(|rx| rx.recv()))
        .await
        .into_iter()
        .find_map(|result| match result {
          Ok(Err(e)) => Some(e),
          Err(e) => Some(e.into()),
          Ok(Ok(())) => None,
        });

      if let Some(err) = init_err {
        for client in self.inner.clients.iter() {
          utils::reset_router_task(client.inner());
        }

        Err(err)
      } else {
        Ok(connect_task)
      }
    }
  }

  /// Close the connection to the Redis server for each client. The returned future resolves when the command has been
  /// written to the socket, not when the connection has been fully closed. Some time after this future resolves the
  /// future returned by [connect](Self::connect) will resolve which indicates that the connection has been fully
  /// closed.
  ///
  /// This function will also close all error, pubsub message, and reconnection event streams on all clients in the
  /// pool.
  fn quit(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move {
      join_all(self.inner.clients.iter().map(|c| c.quit())).await;

      Ok(())
    }
  }
}

#[rm_send_if(feature = "glommio")]
impl HeartbeatInterface for Pool {
  fn enable_heartbeat(
    &self,
    interval: Duration,
    break_on_error: bool,
  ) -> impl Future<Output = FredResult<()>> + Send {
    async move {
      loop {
        sleep(interval).await;

        if let Err(error) = try_join_all(self.inner.clients.iter().map(|c| c.ping::<()>(None))).await {
          if break_on_error {
            return Err(error);
          }
        }
      }
    }
  }
}

#[cfg(feature = "i-acl")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-acl")))]
impl AclInterface for Pool {}
#[cfg(feature = "i-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-client")))]
impl ClientInterface for Pool {}
#[cfg(feature = "i-cluster")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-cluster")))]
impl ClusterInterface for Pool {}
#[cfg(feature = "i-config")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-config")))]
impl ConfigInterface for Pool {}
#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
impl GeoInterface for Pool {}
#[cfg(feature = "i-hashes")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hashes")))]
impl HashesInterface for Pool {}
#[cfg(feature = "i-hyperloglog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hyperloglog")))]
impl HyperloglogInterface for Pool {}
#[cfg(feature = "transactions")]
#[cfg_attr(docsrs, doc(cfg(feature = "transactions")))]
impl TransactionInterface for Pool {}
#[cfg(feature = "i-keys")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-keys")))]
impl KeysInterface for Pool {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl LuaInterface for Pool {}
#[cfg(feature = "i-lists")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-lists")))]
impl ListInterface for Pool {}
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl MemoryInterface for Pool {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl ServerInterface for Pool {}
#[cfg(feature = "i-slowlog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-slowlog")))]
impl SlowlogInterface for Pool {}
#[cfg(feature = "i-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sets")))]
impl SetsInterface for Pool {}
#[cfg(feature = "i-sorted-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sorted-sets")))]
impl SortedSetsInterface for Pool {}
#[cfg(feature = "i-streams")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
impl StreamsInterface for Pool {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl FunctionInterface for Pool {}
#[cfg(feature = "i-redis-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
impl RedisJsonInterface for Pool {}
#[cfg(feature = "i-time-series")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
impl TimeSeriesInterface for Pool {}
#[cfg(feature = "i-redisearch")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
impl RediSearchInterface for Pool {}

#[cfg(not(feature = "glommio"))]
struct ExclusivePoolInner {
  clients: Vec<RefCount<AsyncMutex<Client>>>,
  counter: AtomicUsize,
}

/// A cheaply cloneable round-robin client pool that provides exclusive ownership over the inner clients.
///
/// This interface can be used when callers require exclusive ownership over the connection. For example,
///
/// ```no_run no_compile
/// WATCH foo
/// foo = GET foo
/// if foo > 1:
///   MULTI
///     INCR foo
///     INCR bar
///     INCR baz
///   EXEC
/// ```
///
/// Unlike [RedisPool](crate::clients::Pool), this pooling interface does not directly implement
/// [ClientLike](crate::interfaces::ClientLike). Callers acquire and release clients via the returned
/// [MutexGuard](OwnedMutexGuard).
///
/// ```rust
/// use fred::{
///   clients::{ExclusivePool, Pool},
///   prelude::*,
/// };
///
/// async fn example() -> Result<(), Error> {
///   let builder = Builder::default_centralized();
///   let shared_pool = builder.build_pool(5)?;
///   let exclusive_pool = builder.build_exclusive_pool(5)?;
///   shared_pool.init().await?;
///   exclusive_pool.init().await?;
///
///   // since `RedisPool` implements `ClientLike` we can use most command interfaces directly
///   let foo: Option<String> = shared_pool.set("foo", 1, None, None, false).await?;
///
///   // with an `ExclusivePool` callers acquire and release clients with an async lock guard
///   let results: Option<(i64, i64, i64)> = {
///     let client = exclusive_pool.acquire().await;
///
///     client.watch("foo").await?;
///     if let Some(1) = client.get::<Option<i64>, _>("foo").await? {
///       let trx = client.multi();
///       trx.incr("foo").await?;
///       trx.incr("bar").await?;
///       trx.incr("baz").await?;
///       Some(trx.exec(true).await?)
///     } else {
///       None
///     }
///   };
///   assert_eq!(results, Some((2, 1, 1)));
///
///   Ok(())
/// }
/// ```
///
/// Callers should avoid cloning the inner clients, if possible.
#[cfg(not(feature = "glommio"))]
#[derive(Clone)]
pub struct ExclusivePool {
  inner: RefCount<ExclusivePoolInner>,
}

#[cfg(not(feature = "glommio"))]
impl fmt::Debug for ExclusivePool {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ExclusivePool")
      .field("size", &self.inner.clients.len())
      .finish()
  }
}

#[cfg(not(feature = "glommio"))]
impl ExclusivePool {
  /// Create a new pool without connecting to the server.
  ///
  /// See the [builder](crate::types::Builder) interface for more information.
  pub fn new(
    config: Config,
    perf: Option<PerformanceConfig>,
    connection: Option<ConnectionConfig>,
    policy: Option<ReconnectPolicy>,
    size: usize,
  ) -> Result<Self, Error> {
    if size == 0 {
      Err(Error::new(ErrorKind::Config, "Pool cannot be empty."))
    } else {
      let mut clients = Vec::with_capacity(size);
      for _ in 0 .. size {
        clients.push(RefCount::new(AsyncMutex::new(Client::new(
          config.clone(),
          perf.clone(),
          connection.clone(),
          policy.clone(),
        ))));
      }

      Ok(ExclusivePool {
        inner: RefCount::new(ExclusivePoolInner {
          clients,
          counter: AtomicUsize::new(0),
        }),
      })
    }
  }

  /// Read the clients in the pool.
  pub fn clients(&self) -> &[RefCount<AsyncMutex<Client>>] {
    &self.inner.clients
  }

  /// Connect each client to the server, returning the task driving each connection.
  ///
  /// Use the base [connect](Self::connect) function to return one handle that drives all connections via [join](https://docs.rs/futures/latest/futures/macro.join.html).
  pub async fn connect_pool(&self) -> Vec<ConnectHandle> {
    let mut connect_tasks = Vec::with_capacity(self.inner.clients.len());
    for locked_client in self.inner.clients.iter() {
      connect_tasks.push(locked_client.lock().await.connect());
    }
    connect_tasks
  }

  /// Connect each client to the server.
  ///
  /// This function returns a `JoinHandle` to a task that drives **all** connections via [join](https://docs.rs/futures/latest/futures/macro.join.html).
  ///
  /// See [connect_pool](crate::clients::Pool::connect_pool) for a variation of this function that separates the
  /// connection tasks.
  ///
  /// See [init](Self::init) for an alternative shorthand.
  pub async fn connect(&self) -> ConnectHandle {
    let tasks = self.connect_pool().await;
    tokio::spawn(async move {
      for result in join_all(tasks).await.into_iter() {
        result??;
      }

      Ok(())
    })
  }

  /// Force a reconnection to the server(s) for each client.
  ///
  /// When running against a cluster this function will also refresh the cached cluster routing table.
  pub async fn force_reconnection(&self) -> FredResult<()> {
    let mut fts = Vec::with_capacity(self.inner.clients.len());
    for locked_client in self.inner.clients.iter() {
      let client = locked_client.clone();
      fts.push(async move { client.lock_owned().await.force_reconnection().await });
    }

    try_join_all(fts).await?;
    Ok(())
  }

  /// Wait for all the clients to connect to the server.
  pub async fn wait_for_connect(&self) -> FredResult<()> {
    let mut fts = Vec::with_capacity(self.inner.clients.len());
    for locked_client in self.inner.clients.iter() {
      let client = locked_client.clone();
      fts.push(async move { client.lock().await.wait_for_connect().await });
    }

    try_join_all(fts).await?;
    Ok(())
  }

  /// Initialize a new routing and connection task for each client and wait for them to connect successfully.
  ///
  /// The returned [ConnectHandle](crate::types::ConnectHandle) refers to the task that drives the routing and
  /// connection layer for each client. It will not finish until the max reconnection count is reached.
  ///
  /// Callers can also use [connect](Self::connect) and [wait_for_connect](Self::wait_for_connect) separately if
  /// needed.
  ///
  /// ```rust
  /// use fred::prelude::*;
  ///
  /// #[tokio::main]
  /// async fn main() -> Result<(), Error> {
  ///   let pool = Builder::default_centralized().build_exclusive_pool(5)?;
  ///   let connection_task = pool.init().await?;
  ///
  ///   // ...
  ///
  ///   pool.quit().await?;
  ///   connection_task.await?
  /// }
  /// ```
  pub async fn init(&self) -> FredResult<ConnectHandle> {
    let mut rxs = Vec::with_capacity(self.inner.clients.len());
    for locked_client in self.inner.clients.iter() {
      let mut rx = {
        locked_client
          .lock()
          .await
          .inner
          .notifications
          .connect
          .load()
          .subscribe()
      };

      rxs.push(async move { rx.recv().await });
    }

    let connect_task = self.connect().await;
    let init_err = join_all(rxs).await.into_iter().find_map(|r| match r {
      Ok(Err(e)) => Some(e),
      Err(e) => Some(e.into()),
      _ => None,
    });

    if let Some(err) = init_err {
      for client in self.inner.clients.iter() {
        utils::reset_router_task(client.lock().await.inner());
      }

      Err(err)
    } else {
      Ok(connect_task)
    }
  }

  /// Read the size of the pool.
  pub fn size(&self) -> usize {
    self.inner.clients.len()
  }

  /// Read the client that should run the next command.
  pub async fn acquire(&self) -> OwnedMutexGuard<Client> {
    let mut idx = utils::incr_atomic(&self.inner.counter) % self.inner.clients.len();

    for _ in 0 .. self.inner.clients.len() {
      if let Ok(client) = self.inner.clients[idx].clone().try_lock_owned() {
        return client;
      }

      idx = (idx + 1) % self.inner.clients.len();
    }

    self.inner.clients[idx].clone().lock_owned().await
  }

  /// Update the internal [PerformanceConfig](crate::types::config::PerformanceConfig) on each client in place with
  /// new values.
  pub async fn update_perf_config(&self, config: PerformanceConfig) {
    for client in self.inner.clients.iter() {
      client.lock().await.update_perf_config(config.clone());
    }
  }

  /// Override the DNS resolution logic for all clients in the pool.
  #[cfg(feature = "dns")]
  #[cfg_attr(docsrs, doc(cfg(feature = "dns")))]
  #[allow(refining_impl_trait)]
  pub async fn set_resolver(&self, resolver: RefCount<dyn Resolve>) {
    for client in self.inner.clients.iter() {
      client.lock().await.set_resolver(resolver.clone()).await;
    }
  }

  /// Close the connection to the Redis server for each client. The returned future resolves when the command has been
  /// written to the socket, not when the connection has been fully closed. Some time after this future resolves the
  /// future returned by [connect](Self::connect) will resolve which indicates that the connection has been fully
  /// closed.
  ///
  /// This function will also close all error, pubsub message, and reconnection event streams on all clients in the
  /// pool.
  pub async fn quit(&self) -> FredResult<()> {
    let mut fts = Vec::with_capacity(self.inner.clients.len());
    for locked_client in self.inner.clients.iter() {
      let client = locked_client.clone();
      fts.push(async move { client.lock().await.quit().await });
    }

    join_all(fts).await;
    Ok(())
  }
}
