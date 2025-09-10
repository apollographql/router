use crate::{
  error::{Error, ErrorKind},
  interfaces,
  modules::inner::ClientInner,
  protocol::{
    command::{ClusterErrorKind, Command, CommandKind, RouterCommand},
    connection::{self, Connection, ExclusiveConnection},
    responders,
    responders::ResponseKind,
    types::{ClusterRouting, ProtocolFrame, Server, SlotRange},
    utils as protocol_utils,
  },
  router::{types::ClusterChange, Connections, Router},
  runtime::{Mutex, RefCount},
  types::{config::ClusterDiscoveryPolicy, ClusterStateChange},
  utils as client_utils,
};
use futures::future::{join_all, try_join_all};
use redis_protocol::resp3::types::{BytesFrame as Resp3Frame, FrameKind, Resp3Frame as _Resp3Frame};
use std::{
  collections::{BTreeSet, HashMap, HashSet, VecDeque},
  ops::DerefMut,
};

async fn write_all_nodes(
  inner: &RefCount<ClientInner>,
  writers: &mut HashMap<Server, Connection>,
  frame: &ProtocolFrame,
) -> Vec<Result<Server, Error>> {
  let num_nodes = writers.len();
  let mut write_ft = Vec::with_capacity(num_nodes);
  for (idx, (server, conn)) in writers.iter_mut().enumerate() {
    let frame = frame.clone();
    write_ft.push(async move {
      _debug!(inner, "Writing command to {} ({}/{})", server, idx + 1, num_nodes);

      if let Some(err) = conn.peek_reader_errors().await {
        _debug!(inner, "Error sending command: {:?}", err);
        return Err(err);
      }

      let server = if let Err(err) = conn.write(frame, true, false).await {
        debug!("{}: Error sending frame to socket: {:?}", conn.server, err);
        return Err(err);
      } else {
        server.clone()
      };
      if let Err(err) = conn.flush().await {
        debug!("{}: Error flushing socket: {:?}", conn.server, err);
        Err(err)
      } else {
        Ok(server)
      }
    });
  }

  join_all(write_ft).await
}

/// Read the next non-pubsub frame from all connections concurrently.
async fn read_all_nodes(
  inner: &RefCount<ClientInner>,
  writers: &mut HashMap<Server, Connection>,
  filter: &HashSet<Server>,
) -> Vec<Result<Option<(Server, Resp3Frame)>, Error>> {
  join_all(writers.iter_mut().map(|(server, conn)| async {
    if filter.contains(server) {
      match conn.read_skip_pubsub(inner).await? {
        Some(frame) => Ok(Some((server.clone(), frame))),
        None => Ok(None),
      }
    } else {
      Ok(None)
    }
  }))
  .await
}

/// Find the first error or buffer successful frames into an array.
fn parse_all_responses(results: &[Result<Option<(Server, Resp3Frame)>, Error>]) -> Result<Resp3Frame, Error> {
  let mut responses = Vec::with_capacity(results.len());
  for result in results.iter() {
    match result {
      Ok(Some((_, frame))) => {
        if let Some(err) = protocol_utils::frame_to_error(frame) {
          return Err(err);
        } else {
          responses.push(frame.clone());
        }
      },
      Ok(None) => continue,
      Err(err) => return Err(err.clone()),
    }
  }

  Ok(Resp3Frame::Array {
    data:       responses,
    attributes: None,
  })
}

async fn all_cluster_request_response(
  inner: &RefCount<ClientInner>,
  writers: &mut HashMap<Server, Connection>,
  mut command: Command,
) -> Result<(), Error> {
  let mut out = Ok(());
  let mut disconnect = Vec::new();
  // write to all the cluster nodes, keeping track of which ones failed, then try to read from the ones that
  // succeeded. at the end disconnect from all the nodes that failed writes or reads and return the last error.
  let frame = protocol_utils::encode_frame(inner, &command)?;
  let all_nodes: HashSet<_> = writers.keys().cloned().collect();

  let results = write_all_nodes(inner, writers, &frame).await;
  let write_success: HashSet<_> = results
    .into_iter()
    .filter_map(|r| match r {
      Ok(server) => Some(server),
      Err(e) => {
        out = Err(e);
        None
      },
    })
    .collect();
  let write_failed: Vec<_> = {
    all_nodes
      .difference(&write_success)
      .inspect(|server| {
        disconnect.push((*server).clone());
      })
      .collect()
  };
  if !write_failed.is_empty() {
    _debug!(inner, "Failed sending command to {:?}", write_failed);
  }

  // try to read from all nodes concurrently, keeping track of which ones failed
  let results = read_all_nodes(inner, writers, &write_success).await;
  command.respond_to_caller(parse_all_responses(&results));

  let read_success: HashSet<_> = results
    .into_iter()
    .filter_map(|result| match result {
      Ok(Some((server, _))) => Some(server),
      Ok(None) => None,
      Err(e) => {
        out = Err(e);
        None
      },
    })
    .collect();
  let read_failed: Vec<_> = {
    all_nodes
      .difference(&read_success)
      .inspect(|server| {
        disconnect.push((*server).clone());
      })
      .collect()
  };
  if !read_failed.is_empty() {
    _debug!(inner, "Failed reading responses from {:?}", read_failed);
  }

  // disconnect from all the connections that failed writing or reading
  for server in disconnect.into_iter() {
    let mut conn = match writers.remove(&server) {
      Some(conn) => conn,
      None => continue,
    };

    // the retry buffer is empty since the caller must drain the connection beforehand in this context
    let result = client_utils::timeout(
      async move {
        let _ = conn.close().await;
        Ok::<(), Error>(())
      },
      inner.connection.internal_command_timeout,
    )
    .await;
    if let Err(err) = result {
      _warn!(inner, "Error disconnecting {:?}", err);
    }
  }

  out
}

/// Send a command to all cluster nodes.
///
/// The caller must drain the in-flight buffers before calling this.
pub async fn send_all_cluster_command(
  inner: &RefCount<ClientInner>,
  router: &mut Router,
  command: Command,
) -> Result<(), Error> {
  match router.connections {
    Connections::Clustered {
      connections: ref mut writers,
      ..
    } => all_cluster_request_response(inner, writers, command).await,
    _ => Err(Error::new(ErrorKind::Config, "Expected clustered config.")),
  }
}

pub fn parse_cluster_changes(cluster_state: &ClusterRouting, writers: &HashMap<Server, Connection>) -> ClusterChange {
  let mut old_servers = BTreeSet::new();
  let mut new_servers = BTreeSet::new();
  for server in cluster_state.unique_primary_nodes().into_iter() {
    new_servers.insert(server);
  }
  for server in writers.keys() {
    old_servers.insert(server.clone());
  }
  let add = new_servers.difference(&old_servers).cloned().collect();
  let remove = old_servers.difference(&new_servers).cloned().collect();

  ClusterChange { add, remove }
}

pub fn broadcast_cluster_change(inner: &RefCount<ClientInner>, changes: &ClusterChange) {
  let mut added: Vec<ClusterStateChange> = changes
    .add
    .iter()
    .map(|server| ClusterStateChange::Add(server.clone()))
    .collect();
  let removed: Vec<ClusterStateChange> = changes
    .remove
    .iter()
    .map(|server| ClusterStateChange::Remove(server.clone()))
    .collect();

  let changes = if added.is_empty() && removed.is_empty() {
    vec![ClusterStateChange::Rebalance]
  } else {
    added.extend(removed);
    added
  };

  inner.notifications.broadcast_cluster_change(changes);
}

/// Parse a cluster redirection frame from the provided server, returning the new destination node info.
pub fn parse_cluster_error_frame(
  inner: &RefCount<ClientInner>,
  frame: &Resp3Frame,
  server: &Server,
) -> Result<(ClusterErrorKind, u16, Server), Error> {
  let (kind, slot, server_str) = match frame.as_str() {
    Some(data) => protocol_utils::parse_cluster_error(data)?,
    None => return Err(Error::new(ErrorKind::Protocol, "Invalid cluster error.")),
  };
  let server = match Server::from_parts(&server_str, &server.host) {
    Some(server) => server,
    None => {
      _warn!(inner, "Invalid server field in cluster error: {}", server_str);
      return Err(Error::new(ErrorKind::Protocol, "Invalid cluster redirection error."));
    },
  };

  Ok((kind, slot, server))
}

/// Process a MOVED or ASK error, retrying commands via the command channel if needed.
///
/// Errors returned here should end the router task.
pub fn redirect_command(inner: &RefCount<ClientInner>, server: &Server, mut command: Command, frame: Resp3Frame) {
  // commands are not redirected to replica nodes
  command.use_replica = false;

  let (kind, slot, server) = match parse_cluster_error_frame(inner, &frame, server) {
    Ok(results) => results,
    Err(e) => {
      command.respond_to_caller(Err(e));
      return;
    },
  };

  let command = match kind {
    ClusterErrorKind::Ask => RouterCommand::Ask { slot, server, command },
    ClusterErrorKind::Moved => RouterCommand::Moved { slot, server, command },
  };
  _debug!(inner, "Sending cluster error to command queue.");
  if let Err(e) = interfaces::send_to_router(inner, command) {
    _warn!(inner, "Cannot send ASKED to router channel: {:?}", e);
  }
}

/// Process the response frame in the context of the last command.
///
/// Errors returned here will be logged, but will not close the socket or initiate a reconnect.
pub fn process_response_frame(
  inner: &RefCount<ClientInner>,
  conn: &mut Connection,
  frame: Resp3Frame,
) -> Result<(), Error> {
  _trace!(inner, "Parsing response frame from {}", conn.server);
  let mut command = match conn.buffer.pop_front() {
    Some(command) => command,
    None => {
      _debug!(
        inner,
        "Missing last command from {}. Dropping {:?}.",
        conn.server,
        frame.kind()
      );
      return Ok(());
    },
  };
  _trace!(
    inner,
    "Checking response to {} ({})",
    command.kind.to_str_debug(),
    command.debug_id()
  );
  if command.blocks_connection() {
    conn.blocked = false;
    inner.backchannel.set_unblocked();
  }
  #[cfg(feature = "partial-tracing")]
  let _ = command.traces.network.take();

  if frame.is_redirection() {
    redirect_command(inner, &conn.server, command, frame);
    return Ok(());
  }

  _trace!(inner, "Handling clustered response kind: {:?}", command.response);
  match command.take_response() {
    ResponseKind::Skip | ResponseKind::Respond(None) => Ok(()),
    ResponseKind::Respond(Some(tx)) => responders::respond_to_caller(inner, &conn.server, command, tx, frame),
    ResponseKind::Buffer {
      received,
      expected,
      frames,
      tx,
      index,
      error_early,
    } => responders::respond_buffer(
      inner,
      &conn.server,
      command,
      received,
      expected,
      error_early,
      frames,
      index,
      tx,
      frame,
    ),
    ResponseKind::KeyScan(scanner) => responders::respond_key_scan(inner, &conn.server, command, scanner, frame),
    ResponseKind::ValueScan(scanner) => responders::respond_value_scan(inner, &conn.server, command, scanner, frame),
    ResponseKind::KeyScanBuffered(scanner) => {
      responders::respond_key_scan_buffered(inner, &conn.server, command, scanner, frame)
    },
  }
}

/// Try connecting to any node in the provided `RedisConfig` or `old_servers`.
pub async fn connect_any(
  inner: &RefCount<ClientInner>,
  old_cache: Option<&[SlotRange]>,
) -> Result<ExclusiveConnection, Error> {
  let mut all_servers: BTreeSet<Server> = if let Some(old_cache) = old_cache {
    old_cache.iter().map(|slot_range| slot_range.primary.clone()).collect()
  } else {
    BTreeSet::new()
  };
  all_servers.extend(inner.config.server.hosts());
  _debug!(inner, "Attempting clustered connections to any of {:?}", all_servers);

  let num_servers = all_servers.len();
  let mut last_error = None;
  for (idx, server) in all_servers.into_iter().enumerate() {
    let mut connection = match connection::create(inner, &server, None).await {
      Ok(connection) => connection,
      Err(e) => {
        last_error = Some(e);
        continue;
      },
    };

    if let Err(e) = connection.setup(inner, None).await {
      last_error = Some(e);
      continue;
    }
    _debug!(
      inner,
      "Connected to {} ({}/{})",
      connection.server,
      idx + 1,
      num_servers
    );
    return Ok(connection);
  }

  Err(last_error.unwrap_or(Error::new(ErrorKind::Cluster, "Failed connecting to any cluster node.")))
}

/// Run the `CLUSTER SLOTS` command on the backchannel, creating a new connection if needed.
///
/// This function will attempt to use the existing backchannel connection, if found. Failing that it will
/// try to connect to any of the cluster nodes as identified in the `RedisConfig` or previous cached state.
///
/// If this returns an error then all known cluster nodes are unreachable.
pub async fn cluster_slots_backchannel(
  inner: &RefCount<ClientInner>,
  cache: Option<&ClusterRouting>,
  force_disconnect: bool,
) -> Result<ClusterRouting, Error> {
  if force_disconnect {
    inner.backchannel.check_and_disconnect(inner, None).await;
  }

  let (response, host) = {
    let command: Command = CommandKind::ClusterSlots.into();

    let backchannel_result = {
      // try to use the existing backchannel connection first
      let mut backchannel = inner.backchannel.transport.write().await;
      if let Some(ref mut transport) = backchannel.deref_mut() {
        let default_host = transport.default_host.clone();

        _trace!(inner, "Sending backchannel CLUSTER SLOTS to {}", transport.server);
        client_utils::timeout(
          transport.request_response(command, inner.is_resp3()),
          inner.internal_command_timeout(),
        )
        .await
        .ok()
        .map(|frame| (frame, default_host))
      } else {
        None
      }
    };
    if backchannel_result.is_none() {
      inner.backchannel.check_and_disconnect(inner, None).await;
    }

    // failing the backchannel, try to connect to any of the user-provided hosts or the last known cluster nodes
    let old_cache = if let Some(policy) = inner.cluster_discovery_policy() {
      match policy {
        ClusterDiscoveryPolicy::ConfigEndpoint => None,
        ClusterDiscoveryPolicy::UseCache => cache.map(|cache| cache.slots()),
      }
    } else {
      cache.map(|cache| cache.slots())
    };

    let command: Command = CommandKind::ClusterSlots.into();
    let (frame, host) = if let Some((frame, host)) = backchannel_result {
      let kind = frame.kind();

      if matches!(kind, FrameKind::SimpleError | FrameKind::BlobError) {
        // try connecting to any of the nodes, then try again
        let mut transport = connect_any(inner, old_cache).await?;
        let frame = client_utils::timeout(
          transport.request_response(command, inner.is_resp3()),
          inner.internal_command_timeout(),
        )
        .await?;
        let host = transport.default_host.clone();
        inner.update_backchannel(transport).await;

        (frame, host)
      } else {
        // use the response from the backchannel command
        (frame, host)
      }
    } else {
      // try connecting to any of the nodes, then try again
      let mut transport = connect_any(inner, old_cache).await?;
      let frame = client_utils::timeout(
        transport.request_response(command, inner.is_resp3()),
        inner.internal_command_timeout(),
      )
      .await?;
      let host = transport.default_host.clone();
      inner.update_backchannel(transport).await;

      (frame, host)
    };

    (protocol_utils::frame_to_results(frame)?, host)
  };
  _trace!(inner, "Recv CLUSTER SLOTS response: {:?}", response);
  if response.is_null() {
    inner.backchannel.check_and_disconnect(inner, None).await;
    return Err(Error::new(
      ErrorKind::Protocol,
      "Invalid or missing CLUSTER SLOTS response.",
    ));
  }

  let mut new_cache = ClusterRouting::new();
  _debug!(inner, "Rebuilding cluster state from host: {}", host);
  new_cache.rebuild(inner, response, &host)?;
  Ok(new_cache)
}

/// Check each connection and remove it from the writer map if it's not working.
pub async fn drop_broken_connections(writers: &mut HashMap<Server, Connection>) -> VecDeque<Command> {
  let mut new_writers = HashMap::with_capacity(writers.len());
  let mut buffer = VecDeque::new();

  for (server, mut writer) in writers.drain() {
    if writer.peek_reader_errors().await.is_some() {
      buffer.extend(writer.close().await);
    } else {
      new_writers.insert(server, writer);
    }
  }

  *writers = new_writers;
  buffer
}

/// Run `CLUSTER SLOTS`, update the cached routing table, and modify the connection map.
pub async fn sync(
  inner: &RefCount<ClientInner>,
  connections: &mut HashMap<Server, Connection>,
  cache: &mut ClusterRouting,
  buffer: &mut VecDeque<Command>,
) -> Result<(), Error> {
  _debug!(inner, "Synchronizing cluster state.");

  // force disconnect if connections is empty or any readers have pending errors
  let force_disconnect = connections.is_empty()
    || join_all(connections.values_mut().map(|c| c.peek_reader_errors()))
      .await
      .into_iter()
      .filter(|err| err.is_some())
      .collect::<Vec<_>>()
      .is_empty();

  let state = cluster_slots_backchannel(inner, Some(&*cache), force_disconnect).await?;
  _debug!(inner, "Cluster routing state: {:?}", state.pretty());
  // update the cached routing table
  inner
    .server_state
    .write()
    .kind
    .update_cluster_state(Some(state.clone()));
  *cache = state.clone();

  buffer.extend(drop_broken_connections(connections).await);
  // detect changes to the cluster topology
  let changes = parse_cluster_changes(&state, connections);
  _debug!(inner, "Changing cluster connections: {:?}", changes);
  broadcast_cluster_change(inner, &changes);

  // drop connections that are no longer used
  for removed_server in changes.remove.into_iter() {
    _debug!(inner, "Disconnecting from cluster node {}", removed_server);
    let mut writer = match connections.remove(&removed_server) {
      Some(writer) => writer,
      None => continue,
    };

    let commands = writer.close().await;
    buffer.extend(commands);
  }

  let mut connections_ft = Vec::with_capacity(changes.add.len());
  let new_writers = RefCount::new(Mutex::new(HashMap::with_capacity(changes.add.len())));
  // connect to each of the new nodes concurrently
  for server in changes.add.into_iter() {
    let _inner = inner.clone();
    let _new_writers = new_writers.clone();
    connections_ft.push(async move {
      _debug!(inner, "Connecting to cluster node {}", server);
      let mut transport = connection::create(&_inner, &server, None).await?;
      transport.setup(&_inner, None).await?;
      let connection = transport.into_pipelined(false);
      inner.notifications.broadcast_reconnect(server.clone());
      _new_writers.lock().insert(server, connection);
      Ok::<_, Error>(())
    });
  }

  let _ = try_join_all(connections_ft).await?;
  let mut server_version = None;
  for (server, writer) in new_writers.lock().drain() {
    server_version = writer.version.clone();
    connections.insert(server, writer);
  }

  _debug!(inner, "Finish synchronizing cluster connections.");
  if let Some(version) = server_version {
    inner.server_state.write().kind.set_server_version(version);
  }
  Ok(())
}

/// Initialize fresh connections to the server, dropping any old connections and saving in-flight commands on
/// `buffer`.
pub async fn initialize_connections(
  inner: &RefCount<ClientInner>,
  connections: &mut Connections,
  buffer: &mut VecDeque<Command>,
) -> Result<(), Error> {
  match connections {
    Connections::Clustered {
      connections: ref mut writers,
      ref mut cache,
    } => sync(inner, writers, cache, buffer).await,
    _ => Err(Error::new(ErrorKind::Config, "Expected clustered config.")),
  }
}
