use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::{command::Command, connection, connection::ExclusiveConnection, types::Server},
  router::connections::Connections,
  runtime::{AsyncRwLock, RefCount},
  utils,
};
use parking_lot::Mutex;
use redis_protocol::resp3::types::BytesFrame as Resp3Frame;
use std::{
  collections::HashMap,
  ops::{Deref, DerefMut},
};

/// Check if an existing connection can be used to the provided `server`, otherwise create a new one.
///
/// Returns whether a new connection was created.
async fn check_and_create_transport(
  backchannel: &Backchannel,
  inner: &RefCount<ClientInner>,
  server: &Server,
) -> Result<bool, Error> {
  let mut transport = backchannel.transport.write().await;

  if let Some(ref mut transport) = transport.deref_mut() {
    if &transport.server == server && transport.ping(inner).await.is_ok() {
      _debug!(inner, "Using existing backchannel connection to {}", server);
      return Ok(false);
    }
  }
  *transport.deref_mut() = None;

  let mut _transport = connection::create(inner, server, None).await?;
  _transport.setup(inner, None).await?;
  *transport.deref_mut() = Some(_transport);

  Ok(true)
}

/// A struct wrapping a separate connection to the server or cluster for client or cluster management commands.
pub struct Backchannel {
  /// A connection to any of the servers.
  pub transport:      AsyncRwLock<Option<ExclusiveConnection>>,
  /// An identifier for the blocked connection, if any.
  pub blocked:        Mutex<Option<Server>>,
  /// A map of server IDs to connection IDs, as managed by the router.
  pub connection_ids: Mutex<HashMap<Server, i64>>,
}

impl Default for Backchannel {
  fn default() -> Self {
    Backchannel {
      transport:      AsyncRwLock::new(None),
      blocked:        Mutex::new(None),
      connection_ids: Mutex::new(HashMap::new()),
    }
  }
}

impl Backchannel {
  /// Check if the current server matches the provided server, and disconnect.
  // TODO does this need to disconnect whenever the caller manually changes the RESP protocol mode?
  pub async fn check_and_disconnect(&self, inner: &RefCount<ClientInner>, server: Option<&Server>) {
    let should_close = self
      .current_server()
      .await
      .map(|current| server.map(|server| *server == current).unwrap_or(true))
      .unwrap_or(false);

    if should_close {
      if let Some(ref mut transport) = self.transport.write().await.take() {
        let _ = transport.disconnect(inner).await;
      }
    }
  }

  /// Check if the provided server is marked as blocked, and if so remove it from the cache.
  pub fn check_and_unblock(&self, server: &Server) {
    let mut guard = self.blocked.lock();
    let matches = if let Some(blocked) = guard.as_ref() {
      blocked == server
    } else {
      false
    };

    if matches {
      *guard = None;
    }
  }

  /// Clear all local state that depends on the associated `Router` instance.
  pub async fn clear_router_state(&self, inner: &RefCount<ClientInner>) {
    self.connection_ids.lock().clear();
    self.blocked.lock().take();

    if let Some(ref mut transport) = self.transport.write().await.take() {
      let _ = transport.disconnect(inner).await;
    }
  }

  /// Set the connection IDs from the router.
  pub fn update_connection_ids(&self, connections: &Connections) {
    let mut guard = self.connection_ids.lock();
    *guard.deref_mut() = connections.connection_ids();
  }

  /// Remove the provided server from the connection ID map.
  pub fn remove_connection_id(&self, server: &Server) {
    self.connection_ids.lock().get(server);
  }

  /// Read the connection ID for the provided server.
  pub fn connection_id(&self, server: &Server) -> Option<i64> {
    self.connection_ids.lock().get(server).cloned()
  }

  /// Set the blocked flag to the provided server.
  pub fn set_blocked(&self, server: &Server) {
    self.blocked.lock().replace(server.clone());
  }

  /// Remove the blocked flag.
  pub fn set_unblocked(&self) {
    self.blocked.lock().take();
  }

  /// Remove the blocked flag only if the server matches the blocked server.
  pub fn check_and_set_unblocked(&self, server: &Server) {
    let mut guard = self.blocked.lock();
    if guard.as_ref().map(|b| b == server).unwrap_or(false) {
      guard.take();
    }
  }

  /// Whether the client is blocked on a command.
  pub fn is_blocked(&self) -> bool {
    self.blocked.lock().is_some()
  }

  /// Whether an open connection exists to the blocked server.
  pub async fn has_blocked_transport(&self) -> bool {
    if let Some(server) = self.blocked_server() {
      match self.transport.read().await.deref() {
        Some(ref transport) => transport.server == server,
        None => false,
      }
    } else {
      false
    }
  }

  /// Return the server ID of the blocked client connection, if found.
  pub fn blocked_server(&self) -> Option<Server> {
    self.blocked.lock().clone()
  }

  /// Return the server ID of the existing backchannel connection, if found.
  pub async fn current_server(&self) -> Option<Server> {
    self.transport.read().await.as_ref().map(|t| t.server.clone())
  }

  /// Return a server ID, with the following preferences:
  ///
  /// 1. The server ID of the existing connection, if any.
  /// 2. The blocked server ID, if any.
  /// 3. A random server ID from the router's connection map.
  pub async fn any_server(&self) -> Option<Server> {
    self
      .current_server()
      .await
      .or(self.blocked_server())
      .or_else(|| self.connection_ids.lock().keys().next().cloned())
  }

  /// Whether the existing connection is to the currently blocked server.
  pub async fn current_server_is_blocked(&self) -> bool {
    self
      .current_server()
      .await
      .and_then(|server| self.blocked_server().map(|blocked| server == blocked))
      .unwrap_or(false)
  }

  /// Send the provided command to the provided server, creating a new connection if needed.
  ///
  /// If a new connection is created this function also sets it on `self` before returning.
  pub async fn request_response(
    &self,
    inner: &RefCount<ClientInner>,
    server: &Server,
    command: Command,
  ) -> Result<Resp3Frame, Error> {
    let _ = check_and_create_transport(self, inner, server).await?;

    if let Some(ref mut transport) = self.transport.write().await.deref_mut() {
      _debug!(
        inner,
        "Sending {} ({}) on backchannel to {}",
        command.kind.to_str_debug(),
        command.debug_id(),
        server
      );

      utils::timeout(
        transport.request_response(command, inner.is_resp3()),
        inner.connection_timeout(),
      )
      .await
    } else {
      Err(Error::new(
        ErrorKind::Unknown,
        "Failed to create backchannel connection.",
      ))
    }
  }

  /// Find the server identifier that should receive the provided command.
  ///
  /// Servers are chosen with the following preference order:
  ///
  /// * If `use_blocked` is true and a connection is blocked then that server will be used.
  /// * If the client is clustered and the command uses a hashing policy that specifies a specific server then that
  ///   will be used.
  /// * If a backchannel connection already exists then that will be used.
  /// * Failing all of the above a random server will be used.
  pub async fn find_server(
    &self,
    inner: &RefCount<ClientInner>,
    command: &Command,
    use_blocked: bool,
  ) -> Result<Server, Error> {
    if use_blocked {
      if let Some(server) = self.blocked.lock().deref() {
        Ok(server.clone())
      } else {
        // should this be more relaxed?
        Err(Error::new(ErrorKind::Unknown, "No connections are blocked."))
      }
    } else if inner.config.server.is_clustered() {
      if command.kind.use_random_cluster_node() {
        self
          .any_server()
          .await
          .ok_or_else(|| Error::new(ErrorKind::Unknown, "Failed to find backchannel server."))
      } else {
        inner.with_cluster_state(|state| {
          let slot = match command.cluster_hash() {
            Some(slot) => slot,
            None => return Err(Error::new(ErrorKind::Cluster, "Failed to find cluster hash slot.")),
          };
          state
            .get_server(slot)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Cluster, "Failed to find cluster owner."))
        })
      }
    } else {
      self
        .any_server()
        .await
        .ok_or_else(|| Error::new(ErrorKind::Unknown, "Failed to find backchannel server."))
    }
  }
}
