use crate::{
  error::*,
  modules::backchannel::Backchannel,
  protocol::{
    command::RouterCommand,
    connection::ExclusiveConnection,
    types::{ClusterRouting, DefaultResolver, Resolve, Server},
  },
  runtime::{
    broadcast_channel,
    broadcast_send,
    channel,
    sleep,
    AsyncRwLock,
    AtomicBool,
    AtomicUsize,
    BroadcastSender,
    Mutex,
    Receiver,
    RefCount,
    RefSwap,
    RwLock,
    Sender,
  },
  trace,
  types::{
    config::{ClusterDiscoveryPolicy, Config, ConnectionConfig, PerformanceConfig, ReconnectPolicy, ServerConfig},
    ClientState,
    ClusterStateChange,
    KeyspaceEvent,
    Message,
    RespVersion,
  },
  utils,
};
use bytes_utils::Str;
use futures::future::{select, Either};
use semver::Version;
use std::{ops::DerefMut, time::Duration};

#[cfg(feature = "metrics")]
use crate::modules::metrics::MovingStats;
#[cfg(feature = "credential-provider")]
use crate::{
  clients::Client,
  interfaces::FredResult,
  interfaces::{AuthInterface, ClientLike},
  runtime::{spawn, JoinHandle},
};
#[cfg(feature = "replicas")]
use std::collections::HashMap;
#[cfg(feature = "dynamic-pool")]
use std::time::Instant;

pub type CommandSender = Sender<RouterCommand>;
pub type CommandReceiver = Receiver<RouterCommand>;

#[cfg(feature = "i-tracking")]
use crate::types::client::Invalidation;

pub struct Notifications {
  /// The client ID.
  pub id:             Str,
  /// A broadcast channel for the `on_error` interface.
  pub errors:         RefSwap<RefCount<BroadcastSender<(Error, Option<Server>)>>>,
  /// A broadcast channel for the `on_message` interface.
  pub pubsub:         RefSwap<RefCount<BroadcastSender<Message>>>,
  /// A broadcast channel for the `on_keyspace_event` interface.
  pub keyspace:       RefSwap<RefCount<BroadcastSender<KeyspaceEvent>>>,
  /// A broadcast channel for the `on_reconnect` interface.
  pub reconnect:      RefSwap<RefCount<BroadcastSender<Server>>>,
  /// A broadcast channel for the `on_cluster_change` interface.
  pub cluster_change: RefSwap<RefCount<BroadcastSender<Vec<ClusterStateChange>>>>,
  /// A broadcast channel for the `on_connect` interface.
  pub connect:        RefSwap<RefCount<BroadcastSender<Result<(), Error>>>>,
  /// A channel for events that should close all client tasks with `Canceled` errors.
  ///
  /// Emitted when QUIT, SHUTDOWN, etc are called.
  pub close:          BroadcastSender<()>,
  /// A broadcast channel for the `on_invalidation` interface.
  #[cfg(feature = "i-tracking")]
  pub invalidations:  RefSwap<RefCount<BroadcastSender<Invalidation>>>,
  /// A broadcast channel for notifying callers when servers go unresponsive.
  pub unresponsive:   RefSwap<RefCount<BroadcastSender<Server>>>,
}

impl Notifications {
  pub fn new(id: &Str, capacity: usize) -> Self {
    Notifications {
      id:                                           id.clone(),
      close:                                        broadcast_channel(capacity).0,
      errors:                                       RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
      pubsub:                                       RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
      keyspace:                                     RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
      reconnect:                                    RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
      cluster_change:                               RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
      connect:                                      RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
      #[cfg(feature = "i-tracking")]
      invalidations:                                RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
      unresponsive:                                 RefSwap::new(RefCount::new(broadcast_channel(capacity).0)),
    }
  }

  /// Replace the senders that have public receivers, closing the receivers in the process.
  pub fn close_public_receivers(&self, capacity: usize) {
    utils::swap_new_broadcast_channel(&self.errors, capacity);
    utils::swap_new_broadcast_channel(&self.pubsub, capacity);
    utils::swap_new_broadcast_channel(&self.keyspace, capacity);
    utils::swap_new_broadcast_channel(&self.reconnect, capacity);
    utils::swap_new_broadcast_channel(&self.cluster_change, capacity);
    utils::swap_new_broadcast_channel(&self.connect, capacity);
    #[cfg(feature = "i-tracking")]
    utils::swap_new_broadcast_channel(&self.invalidations, capacity);
    utils::swap_new_broadcast_channel(&self.unresponsive, capacity);
  }

  pub fn broadcast_error(&self, error: Error, server: Option<Server>) {
    broadcast_send(self.errors.load().as_ref(), &(error, server), |(err, _)| {
      debug!("{}: No `on_error` listener. The error was: {err:?}", self.id);
    });
  }

  pub fn broadcast_pubsub(&self, message: Message) {
    broadcast_send(self.pubsub.load().as_ref(), &message, |_| {
      debug!("{}: No `on_message` listeners.", self.id);
    });
  }

  pub fn broadcast_keyspace(&self, event: KeyspaceEvent) {
    broadcast_send(self.keyspace.load().as_ref(), &event, |_| {
      debug!("{}: No `on_keyspace_event` listeners.", self.id);
    });
  }

  pub fn broadcast_reconnect(&self, server: Server) {
    broadcast_send(self.reconnect.load().as_ref(), &server, |_| {
      debug!("{}: No `on_reconnect` listeners.", self.id);
    });
  }

  pub fn broadcast_cluster_change(&self, changes: Vec<ClusterStateChange>) {
    broadcast_send(self.cluster_change.load().as_ref(), &changes, |_| {
      debug!("{}: No `on_cluster_change` listeners.", self.id);
    });
  }

  pub fn broadcast_connect(&self, result: Result<(), Error>) {
    broadcast_send(self.connect.load().as_ref(), &result, |_| {
      debug!("{}: No `on_connect` listeners.", self.id);
    });
  }

  /// Interrupt any tokio `sleep` calls.
  //`ClientInner::wait_with_interrupt` hides the subscription part from callers.
  pub fn broadcast_close(&self) {
    broadcast_send(&self.close, &(), |_| {
      debug!("{}: No `close` listeners.", self.id);
    });
  }

  #[cfg(feature = "i-tracking")]
  pub fn broadcast_invalidation(&self, msg: Invalidation) {
    broadcast_send(self.invalidations.load().as_ref(), &msg, |_| {
      debug!("{}: No `on_invalidation` listeners.", self.id);
    });
  }

  pub fn broadcast_unresponsive(&self, server: Server) {
    broadcast_send(self.unresponsive.load().as_ref(), &server, |_| {
      debug!("{}: No unresponsive listeners", self.id);
    });
  }
}

#[derive(Clone)]
pub struct ClientCounters {
  pub cmd_buffer_len:   RefCount<AtomicUsize>,
  pub redelivery_count: RefCount<AtomicUsize>,
}

impl Default for ClientCounters {
  fn default() -> Self {
    ClientCounters {
      cmd_buffer_len:   RefCount::new(AtomicUsize::new(0)),
      redelivery_count: RefCount::new(AtomicUsize::new(0)),
    }
  }
}

impl ClientCounters {
  pub fn incr_cmd_buffer_len(&self) -> usize {
    utils::incr_atomic(&self.cmd_buffer_len)
  }

  pub fn decr_cmd_buffer_len(&self) -> usize {
    utils::decr_atomic(&self.cmd_buffer_len)
  }

  pub fn incr_redelivery_count(&self) -> usize {
    utils::incr_atomic(&self.redelivery_count)
  }

  pub fn read_cmd_buffer_len(&self) -> usize {
    utils::read_atomic(&self.cmd_buffer_len)
  }

  pub fn read_redelivery_count(&self) -> usize {
    utils::read_atomic(&self.redelivery_count)
  }

  pub fn take_cmd_buffer_len(&self) -> usize {
    utils::set_atomic(&self.cmd_buffer_len, 0)
  }

  pub fn take_redelivery_count(&self) -> usize {
    utils::set_atomic(&self.redelivery_count, 0)
  }

  pub fn reset(&self) {
    utils::set_atomic(&self.cmd_buffer_len, 0);
    utils::set_atomic(&self.redelivery_count, 0);
  }
}

/// Cached state related to the server(s).
pub struct ServerState {
  pub kind:     ServerKind,
  #[cfg(feature = "replicas")]
  pub replicas: HashMap<Server, Server>,
}

impl ServerState {
  pub fn new(config: &Config) -> Self {
    ServerState {
      kind:                                  ServerKind::new(config),
      #[cfg(feature = "replicas")]
      replicas:                              HashMap::new(),
    }
  }

  #[cfg(feature = "replicas")]
  pub fn update_replicas(&mut self, map: HashMap<Server, Server>) {
    self.replicas = map;
  }
}

/// Added state associated with different server deployment types, synchronized by the router task.
pub enum ServerKind {
  Sentinel {
    version:   Option<Version>,
    /// An updated set of known sentinel nodes.
    sentinels: Vec<Server>,
    /// The server host/port resolved from the sentinel nodes, if known.
    primary:   Option<Server>,
  },
  Cluster {
    version: Option<Version>,
    /// The cached cluster routing table.
    cache:   Option<ClusterRouting>,
  },
  Centralized {
    version: Option<Version>,
  },
}

impl ServerKind {
  /// Create a new, empty server state cache.
  pub fn new(config: &Config) -> Self {
    match config.server {
      ServerConfig::Clustered { .. } => ServerKind::Cluster {
        version: None,
        cache:   None,
      },
      ServerConfig::Sentinel { ref hosts, .. } => ServerKind::Sentinel {
        version:   None,
        sentinels: hosts.clone(),
        primary:   None,
      },
      ServerConfig::Centralized { .. } => ServerKind::Centralized { version: None },
      #[cfg(feature = "unix-sockets")]
      ServerConfig::Unix { .. } => ServerKind::Centralized { version: None },
    }
  }

  pub fn set_server_version(&mut self, new_version: Version) {
    match self {
      ServerKind::Cluster { ref mut version, .. } => {
        *version = Some(new_version);
      },
      ServerKind::Centralized { ref mut version, .. } => {
        *version = Some(new_version);
      },
      ServerKind::Sentinel { ref mut version, .. } => {
        *version = Some(new_version);
      },
    }
  }

  pub fn server_version(&self) -> Option<Version> {
    match self {
      ServerKind::Cluster { ref version, .. } => version.clone(),
      ServerKind::Centralized { ref version, .. } => version.clone(),
      ServerKind::Sentinel { ref version, .. } => version.clone(),
    }
  }

  pub fn update_cluster_state(&mut self, state: Option<ClusterRouting>) {
    if let ServerKind::Cluster { ref mut cache, .. } = *self {
      *cache = state;
    }
  }

  pub fn num_cluster_nodes(&self) -> usize {
    if let ServerKind::Cluster { ref cache, .. } = *self {
      cache
        .as_ref()
        .map(|state| state.unique_primary_nodes().len())
        .unwrap_or(1)
    } else {
      1
    }
  }

  pub fn with_cluster_state<F, R>(&self, func: F) -> Result<R, Error>
  where
    F: FnOnce(&ClusterRouting) -> Result<R, Error>,
  {
    if let ServerKind::Cluster { ref cache, .. } = *self {
      if let Some(state) = cache.as_ref() {
        func(state)
      } else {
        Err(Error::new(ErrorKind::Cluster, "Missing cluster routing state."))
      }
    } else {
      Err(Error::new(ErrorKind::Cluster, "Missing cluster routing state."))
    }
  }

  pub fn update_sentinel_primary(&mut self, server: &Server) {
    if let ServerKind::Sentinel { ref mut primary, .. } = *self {
      *primary = Some(server.clone());
    }
  }

  pub fn sentinel_primary(&self) -> Option<Server> {
    if let ServerKind::Sentinel { ref primary, .. } = *self {
      primary.clone()
    } else {
      None
    }
  }

  pub fn update_sentinel_nodes(&mut self, server: &Server, nodes: Vec<Server>) {
    if let ServerKind::Sentinel {
      ref mut sentinels,
      ref mut primary,
      ..
    } = *self
    {
      *primary = Some(server.clone());
      *sentinels = nodes;
    }
  }

  pub fn read_sentinel_nodes(&self, config: &ServerConfig) -> Option<Vec<Server>> {
    if let ServerKind::Sentinel { ref sentinels, .. } = *self {
      if sentinels.is_empty() {
        match config {
          ServerConfig::Sentinel { ref hosts, .. } => Some(hosts.clone()),
          _ => None,
        }
      } else {
        Some(sentinels.clone())
      }
    } else {
      None
    }
  }
}

fn create_resolver(id: &Str) -> RefCount<dyn Resolve> {
  RefCount::new(DefaultResolver::new(id))
}

#[cfg(feature = "credential-provider")]
fn spawn_credential_refresh(client: Client, interval: Duration) -> JoinHandle<FredResult<()>> {
  spawn(async move {
    loop {
      trace!(
        "{}: Waiting {} ms before refreshing credentials.",
        client.inner.id,
        interval.as_millis()
      );
      client.inner.wait_with_interrupt(interval).await?;

      let (username, password) = match client.inner.config.credential_provider {
        Some(ref provider) => match provider.fetch(None).await {
          Ok(creds) => creds,
          Err(e) => {
            warn!("{}: Failed to fetch and refresh credentials: {e:?}", client.inner.id);
            continue;
          },
        },
        None => (None, None),
      };

      if client.state() != ClientState::Connected {
        debug!("{}: Skip credential refresh when disconnected", client.inner.id);
        continue;
      }

      if let Some(password) = password {
        if client.inner.config.version == RespVersion::RESP3 {
          let username = username.unwrap_or("default".into());
          let result = client
            .hello(RespVersion::RESP3, Some((username.into(), password.into())), None)
            .await;

          if let Err(err) = result {
            warn!("{}: Failed to refresh credentials: {err}", client.inner.id);
          }
        } else if let Err(err) = client.auth(username, password).await {
          warn!("{}: Failed to refresh credentials: {err}", client.inner.id);
        }
      }
    }
  })
}

pub struct ClientInner {
  /// An internal lock used to sync certain select operations that should not run concurrently across tasks.
  pub _lock:         Mutex<()>,
  /// The client ID used for logging and the default `CLIENT SETNAME` value.
  pub id:            Str,
  /// Whether the client uses RESP3.
  pub resp3:         RefCount<AtomicBool>,
  /// The state of the underlying connection.
  pub state:         RwLock<ClientState>,
  /// Client configuration options.
  pub config:        RefCount<Config>,
  /// Connection configuration options.
  pub connection:    RefCount<ConnectionConfig>,
  /// Performance config options for the client.
  pub performance:   RefSwap<RefCount<PerformanceConfig>>,
  /// An optional reconnect policy.
  pub policy:        RwLock<Option<ReconnectPolicy>>,
  /// Notification channels for the event interfaces.
  pub notifications: RefCount<Notifications>,
  /// Shared counters.
  pub counters:      ClientCounters,
  /// The DNS resolver to use when establishing new connections.
  pub resolver:      AsyncRwLock<RefCount<dyn Resolve>>,
  /// A backchannel that can be used to control the router connections even while the connections are blocked.
  pub backchannel:   RefCount<Backchannel>,
  /// Server state cache for various deployment types.
  pub server_state:  RwLock<ServerState>,

  /// An mpsc sender for commands to the router.
  pub command_tx: RefSwap<RefCount<CommandSender>>,
  /// Temporary storage for the receiver half of the router command channel.
  pub command_rx: RwLock<Option<CommandReceiver>>,

  /// A handle to a task that refreshes credentials on an interval.
  #[cfg(feature = "credential-provider")]
  pub credentials_task:      RwLock<Option<JoinHandle<FredResult<()>>>>,
  /// Command latency metrics.
  #[cfg(feature = "metrics")]
  pub latency_stats:         RwLock<MovingStats>,
  /// Network latency metrics.
  #[cfg(feature = "metrics")]
  pub network_latency_stats: RwLock<MovingStats>,
  /// Payload size metrics tracking for requests.
  #[cfg(feature = "metrics")]
  pub req_size_stats:        RefCount<RwLock<MovingStats>>,
  /// Payload size metrics tracking for responses
  #[cfg(feature = "metrics")]
  pub res_size_stats:        RefCount<RwLock<MovingStats>>,
  /// The timestamp of the last command sent to the router.
  #[cfg(feature = "dynamic-pool")]
  pub last_command:          RefSwap<RefCount<Instant>>,
}

#[cfg(feature = "credential-provider")]
impl Drop for ClientInner {
  fn drop(&mut self) {
    self.abort_credential_refresh_task();
    // TODO sent quit to the router task if the receiver is held by the routing task
  }
}

impl ClientInner {
  pub fn new(
    config: Config,
    perf: PerformanceConfig,
    connection: ConnectionConfig,
    policy: Option<ReconnectPolicy>,
  ) -> RefCount<ClientInner> {
    let id = Str::from(format!("fred-{}", utils::random_string(10)));
    let resolver = AsyncRwLock::new(create_resolver(&id));
    let (command_tx, command_rx) = channel(connection.max_command_buffer_len);
    let notifications = RefCount::new(Notifications::new(&id, perf.broadcast_channel_capacity));
    let (config, policy) = (RefCount::new(config), RwLock::new(policy));
    let performance = RefSwap::new(RefCount::new(perf));
    let (counters, state) = (ClientCounters::default(), RwLock::new(ClientState::Disconnected));
    let command_rx = RwLock::new(Some(command_rx));
    let backchannel = RefCount::new(Backchannel::default());
    let server_state = RwLock::new(ServerState::new(&config));
    let resp3 = if config.version == RespVersion::RESP3 {
      RefCount::new(AtomicBool::new(true))
    } else {
      RefCount::new(AtomicBool::new(false))
    };
    let connection = RefCount::new(connection);
    let command_tx = RefSwap::new(RefCount::new(command_tx));

    RefCount::new(ClientInner {
      _lock: Mutex::new(()),
      #[cfg(feature = "metrics")]
      latency_stats: RwLock::new(MovingStats::default()),
      #[cfg(feature = "metrics")]
      network_latency_stats: RwLock::new(MovingStats::default()),
      #[cfg(feature = "metrics")]
      req_size_stats: RefCount::new(RwLock::new(MovingStats::default())),
      #[cfg(feature = "metrics")]
      res_size_stats: RefCount::new(RwLock::new(MovingStats::default())),
      #[cfg(feature = "credential-provider")]
      credentials_task: RwLock::new(None),
      #[cfg(feature = "dynamic-pool")]
      last_command: RefSwap::new(RefCount::new(Instant::now())),

      backchannel,
      command_rx,
      server_state,
      command_tx,
      state,
      counters,
      config,
      performance,
      policy,
      resp3,
      notifications,
      resolver,
      connection,
      id,
    })
  }

  pub fn active_connections(&self) -> Vec<Server> {
    self.backchannel.connection_ids.lock().keys().cloned().collect()
  }

  #[cfg(feature = "replicas")]
  pub fn ignore_replica_reconnect_errors(&self) -> bool {
    self.connection.replica.ignore_reconnection_errors
  }

  #[cfg(not(feature = "replicas"))]
  pub fn ignore_replica_reconnect_errors(&self) -> bool {
    true
  }

  /// Swap the command channel sender, returning the old one.
  pub fn swap_command_tx(&self, tx: CommandSender) -> RefCount<CommandSender> {
    self.command_tx.swap(RefCount::new(tx))
  }

  /// Whether the client has the command channel receiver stored. If not then the caller can assume another
  /// connection/router instance is using it.
  pub fn has_command_rx(&self) -> bool {
    self.command_rx.read().is_some()
  }

  pub fn reset_server_state(&self) {
    #[cfg(feature = "replicas")]
    self.server_state.write().replicas.clear()
  }

  pub fn has_unresponsive_duration(&self) -> bool {
    self.connection.unresponsive.max_timeout.is_some()
  }

  pub fn shared_resp3(&self) -> RefCount<AtomicBool> {
    self.resp3.clone()
  }

  pub fn log_client_name_fn<F>(&self, level: log::Level, func: F)
  where
    F: FnOnce(&str),
  {
    if log_enabled!(level) {
      func(&self.id)
    }
  }

  pub async fn set_resolver(&self, resolver: RefCount<dyn Resolve>) {
    let mut guard = self.resolver.write().await;
    *guard = resolver;
  }

  pub fn cluster_discovery_policy(&self) -> Option<&ClusterDiscoveryPolicy> {
    match self.config.server {
      ServerConfig::Clustered { ref policy, .. } => Some(policy),
      _ => None,
    }
  }

  pub async fn get_resolver(&self) -> RefCount<dyn Resolve> {
    self.resolver.write().await.clone()
  }

  pub fn client_name(&self) -> &str {
    &self.id
  }

  pub fn num_cluster_nodes(&self) -> usize {
    self.server_state.read().kind.num_cluster_nodes()
  }

  pub fn with_cluster_state<F, R>(&self, func: F) -> Result<R, Error>
  where
    F: FnOnce(&ClusterRouting) -> Result<R, Error>,
  {
    self.server_state.read().kind.with_cluster_state(func)
  }

  pub fn with_perf_config<F, R>(&self, func: F) -> R
  where
    F: FnOnce(&PerformanceConfig) -> R,
  {
    let guard = self.performance.load();
    func(guard.as_ref())
  }

  #[cfg(feature = "partial-tracing")]
  pub fn should_trace(&self) -> bool {
    self.config.tracing.enabled
  }

  #[cfg(feature = "partial-tracing")]
  pub fn tracing_span_level(&self) -> tracing::Level {
    self.config.tracing.default_tracing_level
  }

  #[cfg(feature = "full-tracing")]
  pub fn full_tracing_span_level(&self) -> tracing::Level {
    self.config.tracing.full_tracing_level
  }

  #[cfg(not(feature = "partial-tracing"))]
  pub fn should_trace(&self) -> bool {
    false
  }

  pub fn take_command_rx(&self) -> Option<CommandReceiver> {
    self.command_rx.write().take()
  }

  pub fn store_command_rx(&self, rx: CommandReceiver, force: bool) {
    let mut guard = self.command_rx.write();
    if guard.is_none() || force {
      *guard = Some(rx);
    }
  }

  pub fn is_resp3(&self) -> bool {
    utils::read_bool_atomic(&self.resp3)
  }

  pub fn switch_protocol_versions(&self, version: RespVersion) {
    match version {
      RespVersion::RESP3 => utils::set_bool_atomic(&self.resp3, true),
      RespVersion::RESP2 => utils::set_bool_atomic(&self.resp3, false),
    };
  }

  pub fn update_performance_config(&self, config: PerformanceConfig) {
    self.performance.store(RefCount::new(config));
  }

  pub fn performance_config(&self) -> PerformanceConfig {
    self.performance.load().as_ref().clone()
  }

  pub fn connection_config(&self) -> ConnectionConfig {
    self.connection.as_ref().clone()
  }

  pub fn reconnect_policy(&self) -> Option<ReconnectPolicy> {
    self.policy.read().as_ref().cloned()
  }

  pub fn reset_protocol_version(&self) {
    let resp3 = match self.config.version {
      RespVersion::RESP3 => true,
      RespVersion::RESP2 => false,
    };

    utils::set_bool_atomic(&self.resp3, resp3);
  }

  pub fn max_command_attempts(&self) -> u32 {
    self.connection.max_command_attempts
  }

  pub fn max_feed_count(&self) -> u64 {
    self.performance.load().max_feed_count
  }

  pub fn default_command_timeout(&self) -> Duration {
    self.performance.load().default_command_timeout
  }

  pub fn connection_timeout(&self) -> Duration {
    self.connection.connection_timeout
  }

  pub fn internal_command_timeout(&self) -> Duration {
    self.connection.internal_command_timeout
  }

  pub async fn set_blocked_server(&self, server: &Server) {
    self.backchannel.blocked.lock().replace(server.clone());
  }

  pub fn should_reconnect(&self) -> bool {
    let has_policy = self
      .policy
      .read()
      .as_ref()
      .map(|policy| policy.should_reconnect())
      .unwrap_or(false);
    let is_disconnecting = utils::read_locked(&self.state) == ClientState::Disconnecting;

    debug!(
      "{}: Checking reconnect state. Has policy: {}, Is intentionally disconnecting: {}",
      self.id, has_policy, is_disconnecting,
    );
    has_policy && !is_disconnecting
  }

  pub fn reset_reconnection_attempts(&self) {
    if let Some(policy) = self.policy.write().deref_mut() {
      policy.reset_attempts();
    }
  }

  pub fn should_cluster_sync(&self, error: &Error) -> bool {
    self.config.server.is_clustered() && error.is_cluster()
  }

  pub async fn update_backchannel(&self, transport: ExclusiveConnection) {
    self.backchannel.transport.write().await.replace(transport);
  }

  pub fn client_state(&self) -> ClientState {
    self.state.read().clone()
  }

  pub fn set_client_state(&self, client_state: ClientState) {
    *self.state.write() = client_state;
  }

  pub fn cas_client_state(&self, expected: ClientState, new_state: ClientState) -> bool {
    let mut state_guard = self.state.write();

    if *state_guard != expected {
      false
    } else {
      *state_guard = new_state;
      true
    }
  }

  pub async fn wait_with_interrupt(&self, duration: Duration) -> Result<(), Error> {
    #[allow(unused_mut)]
    let mut rx = self.notifications.close.subscribe();
    debug!("{}: Sleeping for {} ms", self.id, duration.as_millis());
    let (sleep_ft, recv_ft) = (sleep(duration), rx.recv());
    tokio::pin!(sleep_ft);
    tokio::pin!(recv_ft);

    if let Either::Right((_, _)) = select(sleep_ft, recv_ft).await {
      Err(Error::new(ErrorKind::Canceled, "Connection(s) closed."))
    } else {
      Ok(())
    }
  }

  #[cfg(not(feature = "glommio"))]
  pub fn send_command(self: &RefCount<Self>, command: RouterCommand) -> Result<(), RouterCommand> {
    use tokio::sync::mpsc::error::TrySendError;
    #[cfg(feature = "dynamic-pool")]
    self.last_command.swap(RefCount::new(Instant::now()));

    if let Err(v) = self.command_tx.load().try_send(command) {
      trace!("{}: Failed sending command to router.", self.id);

      match v {
        TrySendError::Closed(c) => Err(c),
        TrySendError::Full(c) => match c {
          RouterCommand::Command(mut cmd) => {
            trace::backpressure_event(&cmd, None);
            cmd.respond_to_caller(Err(Error::new_backpressure()));
            Ok(())
          },
          RouterCommand::Pipeline { mut commands, .. } => {
            if let Some(mut cmd) = commands.pop() {
              cmd.respond_to_caller(Err(Error::new_backpressure()));
            }
            Ok(())
          },
          #[cfg(feature = "transactions")]
          RouterCommand::Transaction { tx, .. } => {
            let _ = tx.send(Err(Error::new_backpressure()));
            Ok(())
          },
          _ => Err(c),
        },
      }
    } else {
      Ok(())
    }
  }

  #[cfg(feature = "glommio")]
  pub fn send_command(self: &RefCount<Self>, command: RouterCommand) -> Result<(), RouterCommand> {
    use glommio::{GlommioError, ResourceType};

    if let Err(e) = self.command_tx.load().try_send(command) {
      match e {
        GlommioError::Closed(ResourceType::Channel(v)) => Err(v),
        GlommioError::WouldBlock(ResourceType::Channel(v)) => match v {
          RouterCommand::Command(mut cmd) => {
            trace::backpressure_event(&cmd, None);
            cmd.respond_to_caller(Err(Error::new_backpressure()));
            Ok(())
          },
          RouterCommand::Pipeline { mut commands, .. } => {
            if let Some(mut cmd) = commands.pop() {
              cmd.respond_to_caller(Err(Error::new_backpressure()));
            }
            Ok(())
          },
          #[cfg(feature = "transactions")]
          RouterCommand::Transaction { tx, .. } => {
            let _ = tx.send(Err(Error::new_backpressure()));
            Ok(())
          },
          _ => Err(v),
        },
        _ => unreachable!(),
      }
    } else {
      Ok(())
    }
  }

  #[cfg(not(feature = "credential-provider"))]
  pub async fn read_credentials(&self, _: &Server) -> Result<(Option<String>, Option<String>), Error> {
    Ok((self.config.username.clone(), self.config.password.clone()))
  }

  #[cfg(feature = "credential-provider")]
  pub async fn read_credentials(&self, server: &Server) -> Result<(Option<String>, Option<String>), Error> {
    Ok(if let Some(ref provider) = self.config.credential_provider {
      provider.fetch(Some(server)).await?
    } else {
      (self.config.username.clone(), self.config.password.clone())
    })
  }

  #[cfg(feature = "credential-provider")]
  pub fn reset_credential_refresh_task(self: &RefCount<Self>) {
    let mut guard = self.credentials_task.write();

    if let Some(task) = guard.take() {
      task.abort();
    }
    let refresh_interval = self
      .config
      .credential_provider
      .as_ref()
      .and_then(|provider| provider.refresh_interval());

    if let Some(interval) = refresh_interval {
      *guard = Some(spawn_credential_refresh(self.into(), interval));
    }
  }

  #[cfg(feature = "credential-provider")]
  pub fn abort_credential_refresh_task(&self) {
    if let Some(task) = self.credentials_task.write().take() {
      task.abort();
    }
  }
}
