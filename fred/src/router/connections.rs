use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::{
    command::Command,
    connection,
    connection::{Connection, Counters},
    types::ClusterRouting,
  },
  router::{centralized, clustered, sentinel},
  runtime::RefCount,
  types::config::Server,
};
use futures::future::try_join_all;
use semver::Version;
use std::collections::{HashMap, VecDeque};

/// Connection maps for the supported deployment types.
pub enum Connections {
  Centralized {
    /// The connection to the primary server.
    connection: Option<Connection>,
  },
  Clustered {
    /// The cached cluster routing table used for mapping keys to server IDs.
    cache:       ClusterRouting,
    /// A map of server IDs and connections.
    connections: HashMap<Server, Connection>,
  },
  Sentinel {
    /// The connection to the primary server.
    connection: Option<Connection>,
  },
}

impl Connections {
  pub fn new_centralized() -> Self {
    Connections::Centralized { connection: None }
  }

  pub fn new_sentinel() -> Self {
    Connections::Sentinel { connection: None }
  }

  pub fn new_clustered() -> Self {
    Connections::Clustered {
      cache:       ClusterRouting::new(),
      connections: HashMap::new(),
    }
  }

  /// Discover and return a mapping of replica nodes to their associated primary node.
  #[cfg(feature = "replicas")]
  pub async fn replica_map(&mut self, inner: &RefCount<ClientInner>) -> Result<HashMap<Server, Server>, Error> {
    Ok(match self {
      Connections::Centralized {
        connection: ref mut writer,
      }
      | Connections::Sentinel {
        connection: ref mut writer,
      } => {
        if let Some(writer) = writer {
          connection::discover_replicas(inner, writer)
            .await?
            .into_iter()
            .map(|replica| (replica, writer.server.clone()))
            .collect()
        } else {
          HashMap::new()
        }
      },
      Connections::Clustered {
        connections: ref writers,
        ..
      } => {
        let mut out = HashMap::with_capacity(writers.len());

        for primary in writers.keys() {
          let replicas = inner
            .with_cluster_state(|state| Ok(state.replicas(primary)))
            .ok()
            .unwrap_or_default();

          for replica in replicas.into_iter() {
            out.insert(replica, primary.clone());
          }
        }
        out
      },
    })
  }

  /// Whether the connection map has a connection to the provided server`.
  pub async fn has_server_connection(&mut self, server: &Server) -> bool {
    match self {
      Connections::Centralized {
        connection: ref mut writer,
      }
      | Connections::Sentinel {
        connection: ref mut writer,
      } => {
        if let Some(writer) = writer.as_mut() {
          if writer.server == *server {
            writer.peek_reader_errors().await.is_none()
          } else {
            false
          }
        } else {
          false
        }
      },
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => {
        for (_, writer) in writers.iter_mut() {
          if writer.server == *server {
            return writer.peek_reader_errors().await.is_none();
          }
        }

        false
      },
    }
  }

  /// Get the connection writer half for the provided server.
  pub fn get_connection_mut(&mut self, server: &Server) -> Option<&mut Connection> {
    match self {
      Connections::Centralized {
        connection: ref mut writer,
      } => writer
        .as_mut()
        .and_then(|writer| if writer.server == *server { Some(writer) } else { None }),
      Connections::Sentinel {
        connection: ref mut writer,
      } => writer
        .as_mut()
        .and_then(|writer| if writer.server == *server { Some(writer) } else { None }),
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => writers.get_mut(server),
    }
  }

  /// Initialize the underlying connection(s) and update the cached backchannel information.
  pub async fn initialize(
    &mut self,
    inner: &RefCount<ClientInner>,
    buffer: &mut VecDeque<Command>,
  ) -> Result<(), Error> {
    let result = if inner.config.server.is_clustered() {
      Box::pin(clustered::initialize_connections(inner, self, buffer)).await
    } else if inner.config.server.is_centralized() || inner.config.server.is_unix_socket() {
      Box::pin(centralized::initialize_connection(inner, self, buffer)).await
    } else if inner.config.server.is_sentinel() {
      Box::pin(sentinel::initialize_connection(inner, self, buffer)).await
    } else {
      return Err(Error::new(ErrorKind::Config, "Invalid client configuration."));
    };

    if result.is_ok() {
      if let Some(version) = self.server_version() {
        inner.server_state.write().kind.set_server_version(version);
      }

      inner.backchannel.update_connection_ids(self);
    }
    result
  }

  /// Read the counters associated with a connection to a server.
  pub fn counters(&self, server: Option<&Server>) -> Option<&Counters> {
    match self {
      Connections::Centralized { connection: ref writer } => writer.as_ref().map(|w| &w.counters),
      Connections::Sentinel {
        connection: ref writer, ..
      } => writer.as_ref().map(|w| &w.counters),
      Connections::Clustered {
        connections: ref writers,
        ..
      } => server.and_then(|server| writers.get(server).map(|w| &w.counters)),
    }
  }

  /// Read the server version, if known.
  pub fn server_version(&self) -> Option<Version> {
    match self {
      Connections::Centralized { connection: ref writer } => writer.as_ref().and_then(|w| w.version.clone()),
      Connections::Clustered {
        connections: ref writers,
        ..
      } => writers.iter().find_map(|(_, w)| w.version.clone()),
      Connections::Sentinel {
        connection: ref writer, ..
      } => writer.as_ref().and_then(|w| w.version.clone()),
    }
  }

  pub fn take_connection(&mut self, server: Option<&Server>) -> Option<Connection> {
    match self {
      Connections::Centralized {
        connection: ref mut writer,
      } => writer.take(),
      Connections::Sentinel {
        connection: ref mut writer,
        ..
      } => writer.take(),
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => server.and_then(|server| writers.remove(server)),
    }
  }

  /// Disconnect from the provided server, using the default centralized connection if `None` is provided.
  pub async fn disconnect(&mut self, inner: &RefCount<ClientInner>, server: Option<&Server>) -> VecDeque<Command> {
    match self {
      Connections::Centralized {
        connection: ref mut writer,
      } => {
        if let Some(mut writer) = writer.take() {
          _debug!(inner, "Disconnecting from {}", writer.server);
          writer.close().await
        } else {
          VecDeque::new()
        }
      },
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => {
        let mut out = VecDeque::new();

        if let Some(server) = server {
          if let Some(mut writer) = writers.remove(server) {
            _debug!(inner, "Disconnecting from {}", writer.server);
            let commands = writer.close().await;
            out.extend(commands);
          }
        }
        out.into_iter().collect()
      },
      Connections::Sentinel {
        connection: ref mut writer,
      } => {
        if let Some(mut writer) = writer.take() {
          _debug!(inner, "Disconnecting from {}", writer.server);
          writer.close().await
        } else {
          VecDeque::new()
        }
      },
    }
  }

  /// Disconnect and clear local state for all connections, returning all in-flight commands.
  pub async fn disconnect_all(&mut self, inner: &RefCount<ClientInner>) -> VecDeque<Command> {
    match self {
      Connections::Centralized {
        connection: ref mut writer,
      } => {
        if let Some(mut writer) = writer.take() {
          _debug!(inner, "Disconnecting from {}", writer.server);
          writer.close().await
        } else {
          VecDeque::new()
        }
      },
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => {
        let mut out = VecDeque::new();
        for (_, mut writer) in writers.drain() {
          _debug!(inner, "Disconnecting from {}", writer.server);
          let commands = writer.close().await;
          out.extend(commands.into_iter());
        }
        out.into_iter().collect()
      },
      Connections::Sentinel {
        connection: ref mut writer,
      } => {
        if let Some(mut writer) = writer.take() {
          _debug!(inner, "Disconnecting from {}", writer.server);
          writer.close().await
        } else {
          VecDeque::new()
        }
      },
    }
  }

  /// Read a map of connection IDs (via `CLIENT ID`) for each inner connections.
  pub fn connection_ids(&self) -> HashMap<Server, i64> {
    let mut out = HashMap::new();

    match self {
      Connections::Centralized { connection: writer } => {
        if let Some(writer) = writer {
          if let Some(id) = writer.id {
            out.insert(writer.server.clone(), id);
          }
        }
      },
      Connections::Sentinel { connection: writer, .. } => {
        if let Some(writer) = writer {
          if let Some(id) = writer.id {
            out.insert(writer.server.clone(), id);
          }
        }
      },
      Connections::Clustered {
        connections: writers, ..
      } => {
        for (server, writer) in writers.iter() {
          if let Some(id) = writer.id {
            out.insert(server.clone(), id);
          }
        }
      },
    }

    out
  }

  /// Flush the socket(s) associated with each server if they have pending frames.
  pub async fn flush(&mut self) -> Result<(), Error> {
    match self {
      Connections::Centralized {
        connection: ref mut writer,
      } => {
        if let Some(writer) = writer {
          writer.flush().await
        } else {
          Ok(())
        }
      },
      Connections::Sentinel {
        connection: ref mut writer,
        ..
      } => {
        if let Some(writer) = writer {
          writer.flush().await
        } else {
          Ok(())
        }
      },
      Connections::Clustered {
        connections: ref mut writers,
        ..
      } => try_join_all(writers.values_mut().map(|writer| writer.flush()))
        .await
        .map(|_| ()),
    }
  }

  /// Check if the provided `server` node owns the provided `slot`.
  pub fn check_cluster_owner(&self, slot: u16, server: &Server) -> bool {
    match self {
      Connections::Clustered { ref cache, .. } => cache
        .get_server(slot)
        .map(|owner| {
          trace!("Comparing cached cluster owner for {}: {} == {}", slot, owner, server);
          owner == server
        })
        .unwrap_or(false),
      _ => false,
    }
  }

  /// Connect or reconnect to the provided `host:port`.
  pub async fn add_connection(&mut self, inner: &RefCount<ClientInner>, server: &Server) -> Result<(), Error> {
    if let Connections::Clustered {
      connections: ref mut writers,
      ..
    } = self
    {
      let mut transport = connection::create(inner, server, None).await?;
      transport.setup(inner, None).await?;
      writers.insert(server.clone(), transport.into_pipelined(false));
      Ok(())
    } else {
      Err(Error::new(ErrorKind::Config, "Expected clustered configuration."))
    }
  }
}
