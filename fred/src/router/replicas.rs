use crate::{
  error::Error,
  modules::inner::ClientInner,
  protocol::{command::Command, connection, connection::Connection},
  runtime::RefCount,
  types::config::Server,
};
use futures::future::join_all;
use std::{
  collections::{HashMap, VecDeque},
  fmt,
  fmt::Formatter,
};

#[cfg(any(feature = "enable-native-tls", feature = "enable-rustls"))]
use crate::types::config::TlsHostMapping;

/// An interface used to filter the list of available replica nodes.
#[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
#[async_trait]
pub trait ReplicaFilter: Send + Sync + 'static {
  /// Returns whether the replica node mapping can be used when routing commands to replicas.
  #[allow(unused_variables)]
  async fn filter(&self, primary: &Server, replica: &Server) -> bool {
    true
  }
}

/// Configuration options for replica node connections.
///
/// When connecting to a replica the client will use the parameters specified in the
/// [ReconnectPolicy](crate::types::config::ReconnectPolicy).
///
/// Currently only clustered replicas are supported.
#[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
#[derive(Clone)]
pub struct ReplicaConfig {
  /// Whether the client should lazily connect to replica nodes.
  ///
  /// Default: `true`
  pub lazy_connections: bool,
  /// An optional interface for filtering available replica nodes.
  ///
  /// Default: `None`
  pub filter: Option<RefCount<dyn ReplicaFilter>>,
  /// Whether the client should ignore errors from replicas that occur when the max reconnection count is reached.
  ///
  /// This implies `primary_fallback: true`.
  ///
  /// Default: `true`
  pub ignore_reconnection_errors: bool,
  /// Whether the client should use the associated primary node if no replica exists that can serve a command.
  ///
  /// Default: `true`
  pub primary_fallback: bool,
}

impl fmt::Debug for ReplicaConfig {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    f.debug_struct("ReplicaConfig")
      .field("lazy_connections", &self.lazy_connections)
      .field("ignore_reconnection_errors", &self.ignore_reconnection_errors)
      .field("primary_fallback", &self.primary_fallback)
      .finish()
  }
}

impl PartialEq for ReplicaConfig {
  fn eq(&self, other: &Self) -> bool {
    self.lazy_connections == other.lazy_connections
      && self.ignore_reconnection_errors == other.ignore_reconnection_errors
      && self.primary_fallback == other.primary_fallback
  }
}

impl Eq for ReplicaConfig {}

impl Default for ReplicaConfig {
  fn default() -> Self {
    ReplicaConfig {
      lazy_connections: true,
      filter: None,
      ignore_reconnection_errors: true,
      primary_fallback: true,
    }
  }
}

/// A container for round-robin routing among replica nodes.
// This implementation optimizes for next() at the cost of add() and remove()
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
pub struct ReplicaRouter {
  counter: usize,
  servers: Vec<Server>,
}

impl ReplicaRouter {
  /// Read the server that should receive the next command.
  pub fn next(&mut self) -> Option<&Server> {
    self.counter = (self.counter + 1) % self.servers.len();
    self.servers.get(self.counter)
  }

  /// Conditionally add the server to the replica set.
  pub fn add(&mut self, server: Server) {
    if !self.servers.contains(&server) {
      self.servers.push(server);
    }
  }

  /// Remove the server from the replica set.
  pub fn remove(&mut self, server: &Server) {
    self.servers = self.servers.drain(..).filter(|_server| server != _server).collect();
  }

  /// The size of the replica set.
  pub fn len(&self) -> usize {
    self.servers.len()
  }

  /// Iterate over the replica set.
  pub fn iter(&self) -> impl Iterator<Item = &Server> {
    self.servers.iter()
  }
}

/// A container for round-robin routing to replica servers.
#[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ReplicaSet {
  /// A map of primary server IDs to a counter and set of replica server IDs.
  servers: HashMap<Server, ReplicaRouter>,
}

impl ReplicaSet {
  /// Create a new empty replica set.
  pub fn new() -> ReplicaSet {
    ReplicaSet {
      servers: HashMap::new(),
    }
  }

  /// Add a replica node to the routing table.
  pub fn add(&mut self, primary: Server, replica: Server) {
    self.servers.entry(primary).or_default().add(replica);
  }

  /// Remove a replica node mapping from the routing table.
  pub fn remove(&mut self, primary: &Server, replica: &Server) {
    let should_remove = if let Some(router) = self.servers.get_mut(primary) {
      router.remove(replica);
      router.len() == 0
    } else {
      false
    };

    if should_remove {
      self.servers.remove(primary);
    }
  }

  /// Remove the replica from all routing sets.
  pub fn remove_replica(&mut self, replica: &Server) {
    self.servers = self
      .servers
      .drain()
      .filter_map(|(primary, mut routing)| {
        routing.remove(replica);

        if routing.len() > 0 {
          Some((primary, routing))
        } else {
          None
        }
      })
      .collect();
  }

  /// Read the server ID of the next replica that should receive a command.
  pub fn next_replica(&mut self, primary: &Server) -> Option<&Server> {
    self.servers.get_mut(primary).and_then(|router| router.next())
  }

  /// Read all the replicas associated with the provided primary node.
  pub fn replicas(&self, primary: &Server) -> impl Iterator<Item = &Server> {
    self
      .servers
      .get(primary)
      .map(|router| router.iter())
      .into_iter()
      .flatten()
  }

  /// Return a map of replica nodes to primary nodes.
  pub fn to_map(&self) -> HashMap<Server, Server> {
    let mut out = HashMap::with_capacity(self.servers.len());
    for (primary, replicas) in self.servers.iter() {
      for replica in replicas.iter() {
        out.insert(replica.clone(), primary.clone());
      }
    }

    out
  }

  /// Clear the routing table.
  pub fn clear(&mut self) {
    self.servers.clear();
  }
}

/// A struct for routing commands to replica nodes.
#[cfg(feature = "replicas")]
pub struct Replicas {
  pub connections: HashMap<Server, Connection>,
  pub routing: ReplicaSet,
  pub buffer: VecDeque<Command>,
}

#[cfg(feature = "replicas")]
#[allow(dead_code)]
impl Replicas {
  pub fn new() -> Replicas {
    Replicas {
      connections: HashMap::new(),
      routing: ReplicaSet::new(),
      buffer: VecDeque::new(),
    }
  }

  /// Sync the connection map in place based on the cached routing table.
  pub async fn sync_connections(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    for (_, mut writer) in self.connections.drain() {
      let commands = writer.close().await;
      self.buffer.extend(commands);
    }

    for (replica, primary) in self.routing.to_map() {
      self.add_connection(inner, primary, replica, false).await?;
    }

    Ok(())
  }

  /// Drop all connections and clear the cached routing table.
  pub async fn clear_connections(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    self.routing.clear();
    self.sync_connections(inner).await
  }

  /// Clear the cached routing table without dropping connections.
  pub fn clear_routing(&mut self) {
    self.routing.clear();
  }

  /// Connect to the replica and add it to the cached routing table.
  pub async fn add_connection(
    &mut self,
    inner: &RefCount<ClientInner>,
    primary: Server,
    replica: Server,
    force: bool,
  ) -> Result<(), Error> {
    _debug!(
      inner,
      "Adding replica connection {} (replica) -> {} (primary)",
      replica,
      primary
    );

    if !inner.connection.replica.lazy_connections || force {
      let mut transport = connection::create(inner, &replica, None).await?;
      transport.setup(inner, None).await?;

      if inner.config.server.is_clustered() {
        transport.readonly(inner, None).await?;
      };

      if let Some(id) = transport.id {
        inner
          .backchannel
          .connection_ids
          .lock()
          .insert(transport.server.clone(), id);
      }
      self.connections.insert(replica.clone(), transport.into_pipelined(true));
      
      // Debug: Verify connection storage
      _debug!(inner, "Successfully stored replica connection {} in HashMap. Total connections: {}", 
              replica, self.connections.len());
      
      if let Some(_stored_conn) = self.connections.get(&replica) {
        _debug!(inner, "Verified replica connection {} exists in HashMap", replica);

        // NOTE: this is where I moved the routing table add--only when new conns are created
        //self.routing.add(primary, replica);
      } else {
        _error!(inner, "Failed to store replica connection {} in HashMap!", replica);
      }
    }

    // NOTE: this is where the old logic added to the routing table
    self.routing.add(primary, replica);
    
    // Debug: Log connection state after creation
    self.log_connection_state(inner, "after add_connection");
    
    Ok(())
  }

  /// Drop the socket associated with the provided server.
  pub async fn drop_writer(&mut self, inner: &RefCount<ClientInner>, replica: &Server) {
    if let Some(mut writer) = self.connections.remove(replica) {
      _warn!(inner, "REMOVING replica connection {} from HashMap. Remaining: {}", 
             replica, self.connections.len());
      self.buffer.extend(writer.close().await);
      inner.backchannel.connection_ids.lock().remove(replica);
    } else {
      _debug!(inner, "Attempted to remove replica connection {} but it wasn't in HashMap", replica);
    }
  }

  /// Remove the replica from the routing table.
  pub fn remove_replica(&mut self, replica: &Server) {
    self.routing.remove_replica(replica);
  }

  /// Close the replica connection and optionally remove the replica from the routing table.
  pub async fn remove_connection(
    &mut self,
    inner: &RefCount<ClientInner>,
    primary: &Server,
    replica: &Server,
    keep_routable: bool,
  ) -> Result<(), Error> {
    _debug!(
      inner,
      "Removing replica connection {} (replica) -> {} (primary)",
      replica,
      primary
    );
    self.drop_writer(inner, replica).await;

    if !keep_routable {
      self.routing.remove(primary, replica);
    }
    Ok(())
  }

  /// Check and flush all the sockets managed by the replica routing state.
  pub async fn flush(&mut self) -> Result<(), Error> {
    for (_, writer) in self.connections.iter_mut() {
      writer.flush().await?;
    }

    Ok(())
  }

  /// Whether a working connection exists to any replica for the provided primary node.
  pub async fn has_replica_connection(&mut self, primary: &Server) -> bool {
    for replica in self.routing.replicas(primary) {
      if let Some(replica) = self.connections.get_mut(replica) {
        if replica.peek_reader_errors().await.is_some() {
          continue;
        } else {
          return true;
        }
      } else {
        continue;
      }
    }

    false
  }

  /// Return a map of `replica` -> `primary` server identifiers.
  pub fn routing_table(&self) -> HashMap<Server, Server> {
    self.routing.to_map()
  }

  /// Check the active connections and drop any without a working reader task.
  pub async fn drop_broken_connections(&mut self) {
    let mut new_writers = HashMap::with_capacity(self.connections.len());
    for (server, mut writer) in self.connections.drain() {
      if writer.peek_reader_errors().await.is_some() {
        self.buffer.extend(writer.close().await);
        self.routing.remove_replica(&server);
      } else {
        new_writers.insert(server, writer);
      }
    }

    self.connections = new_writers;
  }

  /// Read the set of all active connections.
  pub async fn active_connections(&mut self) -> Vec<Server> {
    join_all(self.connections.iter_mut().map(|(server, conn)| async move {
      if conn.peek_reader_errors().await.is_some() {
        None
      } else {
        Some(server.clone())
      }
    }))
    .await
    .into_iter()
    .flatten()
    .collect()
  }

  /// Take the commands stored for retry later.
  pub fn take_retry_buffer(&mut self) -> VecDeque<Command> {
    self.buffer.drain(..).collect()
  }

  /// Log current connection state for debugging.
  pub fn log_connection_state(&self, inner: &RefCount<ClientInner>, context: &str) {
    let routing_replicas: Vec<_> = self.routing.to_map().keys().cloned().collect();
    let connection_replicas: Vec<_> = self.connections.keys().cloned().collect();
    
    _debug!(inner, "Connection state at {}: Routing table has {} replicas: {:?}, HashMap has {} connections: {:?}", 
            context, routing_replicas.len(), routing_replicas, 
            connection_replicas.len(), connection_replicas);
            
    // Check for mismatches
    for replica in &routing_replicas {
        if !self.connections.contains_key(replica) {
            _warn!(inner, "MISMATCH: Replica {} in routing table but NOT in HashMap", replica);
        }
    }
  }

  pub async fn drain(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    // let inner = inner.clone();
    let _ = join_all(self.connections.iter_mut().map(|(_, conn)| conn.drain(inner)))
      .await
      .into_iter()
      .collect::<Result<Vec<()>, Error>>()?;

    Ok(())
  }
}

#[cfg(any(feature = "enable-native-tls", feature = "enable-rustls"))]
pub fn map_replica_tls_names(inner: &RefCount<ClientInner>, primary: &Server, replica: &mut Server) {
  let policy = match inner.config.tls {
    Some(ref config) => &config.hostnames,
    None => {
      _trace!(inner, "Skip modifying TLS hostname for replicas.");
      return;
    },
  };
  if *policy == TlsHostMapping::None {
    _trace!(inner, "Skip modifying TLS hostnames for replicas.");
    return;
  }

  replica.set_tls_server_name(policy, &primary.host);
}

#[cfg(not(any(feature = "enable-native-tls", feature = "enable-rustls")))]
pub fn map_replica_tls_names(_: &RefCount<ClientInner>, _: &Server, _: &mut Server) {}
