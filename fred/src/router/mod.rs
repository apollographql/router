pub mod centralized;
pub mod clustered;
pub mod commands;
pub mod connections;
#[cfg(feature = "replicas")]
pub mod replicas;
pub mod responses;
pub mod sentinel;
pub mod types;
pub mod utils;

use crate::{
  error::Error,
  modules::inner::ClientInner,
  protocol::{
    command::Command,
    connection::{Connection, Counters},
    types::Server,
  },
  router::{
    connections::Connections,
    types::{ReadAllFuture, ReadFuture},
  },
  runtime::RefCount,
  types::Resp3Frame,
  utils as client_utils,
};
use futures::future::join_all;
#[cfg(feature = "replicas")]
use futures::future::try_join;
use std::{
  collections::{HashSet, VecDeque},
  future::pending,
  hash::{Hash, Hasher},
};
#[cfg(feature = "transactions")]
pub mod transactions;
#[cfg(feature = "replicas")]
use replicas::Replicas;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReconnectServer {
  All,
  One(Server),
}

impl Hash for ReconnectServer {
  fn hash<H: Hasher>(&self, state: &mut H) {
    match self {
      ReconnectServer::All => "all".hash(state),
      ReconnectServer::One(server) => server.hash(state),
    }
  }
}

/// A struct for routing commands to the server(s).
pub struct Router {
  /// The connection map for each deployment type.
  pub connections: Connections,
  /// Storage for commands that should be deferred or retried later.
  pub retry_buffer: VecDeque<Command>,
  /// A set to dedup pending reconnection commands.
  pub pending_reconnection: HashSet<ReconnectServer>,
  /// The replica routing interface.
  #[cfg(feature = "replicas")]
  pub replicas: Replicas,
}

impl Router {
  /// Create a new `Router` without connecting to the server(s).
  pub fn new(inner: &RefCount<ClientInner>) -> Self {
    let connections = if inner.config.server.is_clustered() {
      Connections::new_clustered()
    } else if inner.config.server.is_sentinel() {
      Connections::new_sentinel()
    } else {
      Connections::new_centralized()
    };

    Router {
      retry_buffer: VecDeque::new(),
      pending_reconnection: HashSet::new(),
      connections,
      #[cfg(feature = "replicas")]
      replicas: Replicas::new(),
    }
  }

  /// Find the primary node that owns the hash slot used by the command.
  #[cfg(feature = "replicas")]
  pub fn cluster_owner(&self, command: &Command) -> Option<&Server> {
    match self.connections {
      Connections::Clustered { ref cache, .. } => command.cluster_hash().and_then(|slot| cache.get_server(slot)),
      _ => None,
    }
  }

  /// Whether a deferred reconnection command exists for the provided server.
  pub fn has_pending_reconnection(&self, server: &Option<&Server>) -> bool {
    match server {
      Some(server) => {
        self.pending_reconnection.contains(&ReconnectServer::All)
          || self
            .pending_reconnection
            .contains(&ReconnectServer::One((*server).clone()))
      },
      None => self.pending_reconnection.contains(&ReconnectServer::All),
    }
  }

  pub fn reset_pending_reconnection(&mut self, server: Option<&Server>) {
    if let Some(server) = server {
      self.pending_reconnection.remove(&ReconnectServer::One(server.clone()));
    } else {
      self.pending_reconnection.clear();
    }
  }

  /// Find the connection that should receive the provided command.
  #[cfg(feature = "replicas")]
  pub fn route(&mut self, command: &Command) -> Option<&mut Connection> {
    if command.is_all_cluster_nodes() {
      return None;
    }

    match command.cluster_node.as_ref() {
      Some(server) => {
        if command.use_replica {
          self
            .replicas
            .routing
            .next_replica(server)
            .and_then(|replica| self.replicas.connections.get_mut(replica))
        } else {
          self.connections.get_connection_mut(server)
        }
      },
      None => {
        if command.use_replica {
          match self.cluster_owner(command).cloned() {
            Some(primary) => match self.replicas.routing.next_replica(&primary) {
              Some(replica) => self.replicas.connections.get_mut(replica),
              None => None,
            },
            None => None,
          }
        } else {
          match self.connections {
            Connections::Centralized {
              connection: ref mut writer,
            } => writer.as_mut(),
            Connections::Sentinel {
              connection: ref mut writer,
            } => writer.as_mut(),
            Connections::Clustered {
              connections: ref mut writers,
              ref cache,
            } => {
              let server = command.cluster_hash().and_then(|slot| cache.get_server(slot));
              let has_server = server.map(|server| writers.contains_key(server)).unwrap_or(false);

              if has_server {
                server.and_then(|server| writers.get_mut(server))
              } else {
                writers.values_mut().next()
              }
            },
          }
        }
      },
    }
  }

  /// Find the connection that should receive the provided command.
  #[cfg(not(feature = "replicas"))]
  pub fn route<'a>(&'a mut self, command: &Command) -> Option<&'a mut Connection> {
    if command.is_all_cluster_nodes() {
      return None;
    }

    match command.cluster_node.as_ref() {
      Some(server) => self.connections.get_connection_mut(server),
      None => match self.connections {
        Connections::Centralized {
          connection: ref mut writer,
          ..
        } => writer.as_mut(),
        Connections::Sentinel {
          connection: ref mut writer,
          ..
        } => writer.as_mut(),
        Connections::Clustered {
          connections: ref mut writers,
          ref cache,
        } => {
          let server = command.cluster_hash().and_then(|slot| cache.get_server(slot));
          let has_server = server.map(|server| writers.contains_key(server)).unwrap_or(false);

          if has_server {
            server.and_then(|server| writers.get_mut(server))
          } else {
            writers.values_mut().next()
          }
        },
      },
    }
  }

  #[cfg(feature = "replicas")]
  pub fn get_connection_mut(&mut self, server: &Server) -> Option<&mut Connection> {
    self
      .connections
      .get_connection_mut(server)
      .or_else(|| self.replicas.connections.get_mut(server))
  }

  #[cfg(not(feature = "replicas"))]
  pub fn get_connection_mut(&mut self, server: &Server) -> Option<&mut Connection> {
    self.connections.get_connection_mut(server)
  }

  #[cfg(feature = "replicas")]
  pub fn take_connection(&mut self, server: &Server) -> Option<Connection> {
    self
      .connections
      .take_connection(Some(server))
      .or_else(|| self.replicas.connections.remove(server))
  }

  #[cfg(not(feature = "replicas"))]
  pub fn take_connection(&mut self, server: &Server) -> Option<Connection> {
    self.connections.take_connection(Some(server))
  }

  /// Disconnect from all the servers, moving the in-flight messages to the internal command buffer and triggering a
  /// reconnection, if necessary.
  pub async fn disconnect_all(&mut self, inner: &RefCount<ClientInner>) {
    let commands = self.connections.disconnect_all(inner).await;
    self.retry_commands(commands);
    self.disconnect_replicas(inner).await;
  }

  /// Disconnect from all the servers, moving the in-flight messages to the internal command buffer and triggering a
  /// reconnection, if necessary.
  #[cfg(feature = "replicas")]
  pub async fn disconnect_replicas(&mut self, inner: &RefCount<ClientInner>) {
    if let Err(e) = self.replicas.clear_connections(inner).await {
      _warn!(inner, "Error disconnecting replicas: {:?}", e);
    }
  }

  #[cfg(not(feature = "replicas"))]
  pub async fn disconnect_replicas(&mut self, _: &RefCount<ClientInner>) {}

  /// Add the provided commands to the retry buffer.
  pub fn retry_commands(&mut self, commands: impl IntoIterator<Item = Command>) {
    for command in commands.into_iter() {
      self.retry_command(command);
    }
  }

  /// Add the provided command to the retry buffer.
  pub fn retry_command(&mut self, command: Command) {
    self.retry_buffer.push_back(command);
  }

  /// Clear all the commands in the retry buffer.
  pub fn clear_retry_buffer(&mut self) {
    self.retry_buffer.clear();
  }

  /// Connect to the server(s), discarding any previous connection state.
  pub async fn connect(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    let result = self.connections.initialize(inner, &mut self.retry_buffer).await;

    if result.is_ok() {
      #[cfg(feature = "replicas")]
      self.refresh_replica_routing(inner).await?;

      Ok(())
    } else {
      result
    }
  }

  /// Gracefully reset the replica routing table.
  #[cfg(feature = "replicas")]
  pub async fn refresh_replica_routing(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    self.replicas.clear_routing();
    if let Err(e) = self.sync_replicas(inner).await {
      if !inner.ignore_replica_reconnect_errors() {
        return Err(e);
      }
    }

    Ok(())
  }

  /// Sync the cached cluster state with the server via `CLUSTER SLOTS`.
  ///
  /// This will also create new connections or drop old connections as needed.
  pub async fn sync_cluster(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    let result = match self.connections {
      Connections::Clustered {
        connections: ref mut writers,
        ref mut cache,
      } => {
        let result = clustered::sync(inner, writers, cache, &mut self.retry_buffer).await;

        if result.is_ok() {
          #[cfg(feature = "replicas")]
          self.refresh_replica_routing(inner).await?;

          // surface errors from the retry process, otherwise return the reconnection result
          Box::pin(self.retry_buffer(inner)).await?;
        }
        result
      },
      _ => Ok(()),
    };

    inner.backchannel.update_connection_ids(&self.connections);
    result
  }

  /// Rebuild the cached replica routing table based on the primary node connections.
  #[cfg(feature = "replicas")]
  pub async fn sync_replicas(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    _debug!(inner, "Syncing replicas...");
    self.replicas.drop_broken_connections().await;
    let old_connections = self.replicas.active_connections().await;
    let new_replica_map = self.connections.replica_map(inner).await?;

    let old_connections_idx: HashSet<_> = old_connections.iter().collect();
    let new_connections_idx: HashSet<_> = new_replica_map.keys().collect();
    let remove: Vec<_> = old_connections_idx.difference(&new_connections_idx).collect();

    for server in remove.into_iter() {
      _debug!(inner, "Dropping replica connection to {}", server);
      self.replicas.drop_writer(inner, server).await;
      self.replicas.remove_replica(server);
    }

    for (mut replica, primary) in new_replica_map.into_iter() {
      let should_use = if let Some(filter) = inner.connection.replica.filter.as_ref() {
        filter.filter(&primary, &replica).await
      } else {
        true
      };

      if should_use {
        replicas::map_replica_tls_names(inner, &primary, &mut replica);

        self.replicas.add_connection(inner, primary, replica, false).await?;
      }
    }

    inner
      .server_state
      .write()
      .update_replicas(self.replicas.routing_table());
    Ok(())
  }

  /// Attempt to replay all queued commands on the internal buffer without backpressure.
  pub async fn retry_buffer(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    #[cfg(feature = "replicas")]
    {
      let commands = self.replicas.take_retry_buffer();
      self.retry_buffer.extend(commands);
    }

    while let Some(command) = self.retry_buffer.pop_front() {
      if client_utils::read_bool_atomic(&command.timed_out) {
        _debug!(
          inner,
          "Ignore retrying timed out command: {}",
          command.kind.to_str_debug()
        );
        continue;
      }

      _trace!(
        inner,
        "Retry `{}` ({}) command, attempts remaining: {}",
        command.kind.to_str_debug(),
        command.debug_id(),
        command.attempts_remaining,
      );
      if let Err(err) = Box::pin(commands::write_command(inner, self, command)).await {
        _debug!(inner, "Error retrying command: {:?}", err);
        break;
      }
    }

    let _ = self.flush().await;
    Ok(())
  }

  /// Wait and read frames until there are no in-flight frames on primary connections.
  pub async fn drain_all(&mut self, inner: &RefCount<ClientInner>) -> Result<(), Error> {
    let inner = inner.clone();
    _trace!(inner, "Draining all connections...");
    self.flush().await?;

    let primary_ft = async {
      match self.connections {
        Connections::Clustered {
          connections: ref mut writers,
          ..
        } => {
          // drain all connections even if one of them breaks out early with an error
          let _ = join_all(writers.iter_mut().map(|(_, conn)| conn.drain(&inner)))
            .await
            .into_iter()
            .collect::<Result<Vec<()>, Error>>()?;

          Ok(())
        },
        Connections::Centralized {
          connection: ref mut writer,
        }
        | Connections::Sentinel {
          connection: ref mut writer,
        } => match writer {
          Some(ref mut conn) => conn.drain(&inner).await,
          None => Ok(()),
        },
      }
    };

    #[cfg(feature = "replicas")]
    return try_join(primary_ft, self.replicas.drain(&inner)).await.map(|_| ());
    #[cfg(not(feature = "replicas"))]
    primary_ft.await
  }

  pub async fn flush(&mut self) -> Result<(), Error> {
    self.connections.flush().await?;
    #[cfg(feature = "replicas")]
    self.replicas.flush().await?;
    Ok(())
  }

  pub async fn has_healthy_centralized_connection(&mut self) -> bool {
    match self.connections {
      Connections::Centralized {
        connection: ref mut writer,
      }
      | Connections::Sentinel {
        connection: ref mut writer,
      } => {
        if let Some(writer) = writer {
          writer.peek_reader_errors().await.is_none()
        } else {
          false
        }
      },
      _ => false,
    }
  }

  /// Try to read from all sockets concurrently in a select loop.
  #[cfg(feature = "replicas")]
  pub async fn select_read(
    &mut self,
    inner: &RefCount<ClientInner>,
  ) -> Vec<(Server, Option<Result<Resp3Frame, Error>>)> {
    match self.connections {
      Connections::Centralized {
        connection: ref mut writer,
      }
      | Connections::Sentinel {
        connection: ref mut writer,
      } => {
        if let Some(writer) = writer {
          ReadFuture::new(inner, writer, &mut self.replicas.connections).await
        } else {
          pending().await
        }
      },
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => ReadAllFuture::new(inner, writers, &mut self.replicas.connections).await,
    }
  }

  /// Try to read from all sockets concurrently in a select loop.
  #[cfg(not(feature = "replicas"))]
  pub async fn select_read(
    &mut self,
    inner: &RefCount<ClientInner>,
  ) -> Vec<(Server, Option<Result<Resp3Frame, Error>>)> {
    match self.connections {
      Connections::Centralized {
        connection: ref mut writer,
      }
      | Connections::Sentinel {
        connection: ref mut writer,
      } => {
        if let Some(writer) = writer {
          ReadFuture::new(inner, writer).await
        } else {
          pending().await
        }
      },
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => ReadAllFuture::new(inner, writers).await,
    }
  }

  #[cfg(feature = "replicas")]
  pub fn is_replica(&self, server: &Server) -> bool {
    self.replicas.connections.contains_key(server)
  }

  #[cfg(not(feature = "replicas"))]
  pub fn is_replica(&self, _: &Server) -> bool {
    false
  }
}
