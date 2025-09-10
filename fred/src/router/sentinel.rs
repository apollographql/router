#![allow(dead_code)]
use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::{
    command::{Command, CommandKind},
    connection::{self, Connection, ExclusiveConnection},
    utils as protocol_utils,
  },
  router::connections::Connections,
  runtime::RefCount,
  types::{
    config::{Server, ServerConfig},
    Value,
  },
  utils,
};
use bytes_utils::Str;
use std::collections::{HashMap, HashSet, VecDeque};

pub static CONFIG: &str = "CONFIG";
pub static SET: &str = "SET";
pub static CKQUORUM: &str = "CKQUORUM";
pub static FLUSHCONFIG: &str = "FLUSHCONFIG";
pub static FAILOVER: &str = "FAILOVER";
pub static GET_MASTER_ADDR_BY_NAME: &str = "GET-MASTER-ADDR-BY-NAME";
pub static INFO_CACHE: &str = "INFO-CACHE";
pub static MASTERS: &str = "MASTERS";
pub static MASTER: &str = "MASTER";
pub static MONITOR: &str = "MONITOR";
pub static MYID: &str = "MYID";
pub static PENDING_SCRIPTS: &str = "PENDING-SCRIPTS";
pub static REMOVE: &str = "REMOVE";
pub static REPLICAS: &str = "REPLICAS";
pub static SENTINELS: &str = "SENTINELS";
pub static SIMULATE_FAILURE: &str = "SIMULATE-FAILURE";

macro_rules! stry (
  ($expr:expr) => {
    match $expr {
      Ok(r) => r,
      Err(mut e) => {
        e.change_kind(ErrorKind::Sentinel);
        return Err(e);
      }
    }
  }
);

fn parse_sentinel_nodes_response(inner: &RefCount<ClientInner>, value: Value) -> Result<Vec<Server>, Error> {
  let result_maps: Vec<HashMap<String, String>> = stry!(value.convert());
  let mut out = Vec::with_capacity(result_maps.len());

  for mut map in result_maps.into_iter() {
    let ip = match map.remove("ip") {
      Some(ip) => ip,
      None => {
        _warn!(inner, "Failed to read IP for sentinel node.");
        return Err(Error::new(
          ErrorKind::Sentinel,
          "Failed to read sentinel node IP address.",
        ));
      },
    };
    let port = match map.get("port") {
      Some(port) => port.parse::<u16>()?,
      None => {
        _warn!(inner, "Failed to read port for sentinel node.");
        return Err(Error::new(ErrorKind::Sentinel, "Failed to read sentinel node port."));
      },
    };

    out.push(Server::new(ip, port));
  }
  Ok(out)
}

fn has_different_sentinel_nodes(old: &[(String, u16)], new: &[(String, u16)]) -> bool {
  let mut old_set = HashSet::with_capacity(old.len());
  let mut new_set = HashSet::with_capacity(new.len());

  for (host, port) in old.iter() {
    old_set.insert((host, port));
  }
  for (host, port) in new.iter() {
    new_set.insert((host, port));
  }

  old_set.symmetric_difference(&new_set).count() > 0
}

#[cfg(feature = "sentinel-auth")]
fn read_sentinel_auth(inner: &RefCount<ClientInner>) -> Result<(Option<String>, Option<String>), Error> {
  match inner.config.server {
    ServerConfig::Sentinel {
      ref username,
      ref password,
      ..
    } => Ok((username.clone(), password.clone())),
    _ => Err(Error::new(ErrorKind::Config, "Expected sentinel server configuration.")),
  }
}

#[cfg(not(feature = "sentinel-auth"))]
fn read_sentinel_auth(inner: &RefCount<ClientInner>) -> Result<(Option<String>, Option<String>), Error> {
  Ok((inner.config.username.clone(), inner.config.password.clone()))
}

fn read_sentinel_hosts(inner: &RefCount<ClientInner>) -> Result<Vec<Server>, Error> {
  inner
    .server_state
    .read()
    .kind
    .read_sentinel_nodes(&inner.config.server)
    .ok_or(Error::new(ErrorKind::Sentinel, "Failed to read cached sentinel nodes."))
}

/// Read the `(host, port)` tuples for the known sentinel nodes, and the credentials to use when connecting.
#[cfg(feature = "credential-provider")]
async fn read_sentinel_credentials(
  inner: &RefCount<ClientInner>,
  server: &Server,
) -> Result<(Option<String>, Option<String>), Error> {
  let (username, password) = if let Some(ref provider) = inner.config.credential_provider {
    provider.fetch(Some(server)).await?
  } else {
    read_sentinel_auth(inner)?
  };

  Ok((username, password))
}

#[cfg(not(feature = "credential-provider"))]
async fn read_sentinel_credentials(
  inner: &RefCount<ClientInner>,
  _: &Server,
) -> Result<(Option<String>, Option<String>), Error> {
  read_sentinel_auth(inner)
}

/// Read the set of sentinel nodes via `SENTINEL sentinels`.
async fn read_sentinels(
  inner: &RefCount<ClientInner>,
  sentinel: &mut ExclusiveConnection,
) -> Result<Vec<Server>, Error> {
  let service_name = read_service_name(inner)?;

  let command = Command::new(CommandKind::Sentinel, vec![static_val!(SENTINELS), service_name.into()]);
  let frame = sentinel.request_response(command, false).await?;
  let response = stry!(protocol_utils::frame_to_results(frame));
  _trace!(inner, "Read sentinel `sentinels` response: {:?}", response);
  let sentinel_nodes = stry!(parse_sentinel_nodes_response(inner, response));

  if sentinel_nodes.is_empty() {
    _warn!(inner, "Failed to read any known sentinel nodes.")
  }

  Ok(sentinel_nodes)
}

/// Connect to any of the sentinel nodes provided on the associated `RedisConfig`.
async fn connect_to_sentinel(inner: &RefCount<ClientInner>) -> Result<ExclusiveConnection, Error> {
  let hosts = read_sentinel_hosts(inner)?;

  for server in hosts.into_iter() {
    let (username, password) = read_sentinel_credentials(inner, &server).await?;

    _debug!(inner, "Connecting to sentinel {}", server);
    let mut transport = try_or_continue!(connection::create(inner, &server, None).await);
    try_or_continue!(
      utils::timeout(
        transport.authenticate(&inner.id, username.clone(), password.clone(), false),
        inner.internal_command_timeout()
      )
      .await
    );

    return Ok(transport);
  }

  Err(Error::new(
    ErrorKind::Sentinel,
    "Failed to connect to all sentinel nodes.",
  ))
}

fn read_service_name(inner: &RefCount<ClientInner>) -> Result<String, Error> {
  match inner.config.server {
    ServerConfig::Sentinel { ref service_name, .. } => Ok(service_name.to_owned()),
    _ => Err(Error::new(ErrorKind::Sentinel, "Missing sentinel service name.")),
  }
}

/// Read the `(host, port)` tuple for the primary Redis server, as identified by the `SENTINEL
/// get-master-addr-by-name` command, then return a connection to that node.
async fn discover_primary_node(
  inner: &RefCount<ClientInner>,
  sentinel: &mut ExclusiveConnection,
) -> Result<ExclusiveConnection, Error> {
  let service_name = read_service_name(inner)?;
  let command = Command::new(CommandKind::Sentinel, vec![
    static_val!(GET_MASTER_ADDR_BY_NAME),
    service_name.into(),
  ]);
  let frame = utils::timeout(
    sentinel.request_response(command, false),
    inner.internal_command_timeout(),
  )
  .await?;
  let response = stry!(protocol_utils::frame_to_results(frame));
  let server = if response.is_null() {
    return Err(Error::new(
      ErrorKind::Sentinel,
      "Missing primary address in response from sentinel node.",
    ));
  } else {
    let (host, port): (Str, u16) = stry!(response.convert());
    Server {
      host,
      port,
      #[cfg(any(
        feature = "enable-rustls",
        feature = "enable-native-tls",
        feature = "enable-rustls-ring"
      ))]
      tls_server_name: None,
    }
  };

  let mut transport = stry!(connection::create(inner, &server, None).await);
  stry!(transport.setup(inner, None).await);
  Ok(transport)
}

/// Verify that the Redis server is a primary node and not a replica.
async fn check_primary_node_role(
  inner: &RefCount<ClientInner>,
  transport: &mut ExclusiveConnection,
) -> Result<(), Error> {
  let command = Command::new(CommandKind::Role, Vec::new());
  _debug!(inner, "Checking role for redis server at {}", transport.server);

  let frame = stry!(transport.request_response(command, inner.is_resp3()).await);
  let response = stry!(protocol_utils::frame_to_results(frame));

  if let Value::Array(values) = response {
    if let Some(first) = values.first() {
      let is_master = first.as_str().map(|s| s == "master").unwrap_or(false);

      if is_master {
        Ok(())
      } else {
        Err(Error::new(
          ErrorKind::Sentinel,
          format!("Invalid role: {:?}", first.as_str()),
        ))
      }
    } else {
      Err(Error::new(ErrorKind::Sentinel, "Invalid role response."))
    }
  } else {
    Err(Error::new(ErrorKind::Sentinel, "Could not read redis server role."))
  }
}

/// Update the cached backchannel state with the new connection information, disconnecting the old connection if
/// needed.
async fn update_sentinel_backchannel(
  inner: &RefCount<ClientInner>,
  transport: &ExclusiveConnection,
) -> Result<(), Error> {
  inner
    .backchannel
    .check_and_disconnect(inner, Some(&transport.server))
    .await;
  inner.backchannel.connection_ids.lock().clear();
  if let Some(id) = transport.id {
    inner
      .backchannel
      .connection_ids
      .lock()
      .insert(transport.server.clone(), id);
  }

  Ok(())
}

/// Update the cached client and connection state with the latest sentinel state.
///
/// This does the following:
/// * Call `SENTINEL sentinels` on the sentinels
/// * Store the updated sentinel node list on `inner`.
/// * Update the primary node on `inner`.
/// * Update the cached backchannel information.
/// * Split and store the primary node transport on `writer`.
async fn update_cached_client_state(
  inner: &RefCount<ClientInner>,
  writer: &mut Option<Connection>,
  mut sentinel: ExclusiveConnection,
  transport: ExclusiveConnection,
) -> Result<(), Error> {
  let sentinels = read_sentinels(inner, &mut sentinel).await?;
  inner
    .server_state
    .write()
    .kind
    .update_sentinel_nodes(&transport.server, sentinels);
  let _ = update_sentinel_backchannel(inner, &transport).await;

  *writer = Some(transport.into_pipelined(false));
  Ok(())
}

/// Initialize fresh connections to the server, dropping any old connections and saving in-flight commands on
/// `buffer`.
///
/// <https://redis.io/docs/reference/sentinel-clients/>
pub async fn initialize_connection(
  inner: &RefCount<ClientInner>,
  connections: &mut Connections,
  buffer: &mut VecDeque<Command>,
) -> Result<(), Error> {
  _debug!(inner, "Initializing sentinel connection.");
  let commands = connections.disconnect_all(inner).await;
  buffer.extend(commands);

  match connections {
    Connections::Sentinel { connection: writer } => {
      let mut sentinel = connect_to_sentinel(inner).await?;
      let mut transport = discover_primary_node(inner, &mut sentinel).await?;
      let server = transport.server.clone();

      utils::timeout(
        Box::pin(async {
          check_primary_node_role(inner, &mut transport).await?;
          update_cached_client_state(inner, writer, sentinel, transport).await?;
          Ok::<_, Error>(())
        }),
        inner.internal_command_timeout(),
      )
      .await?;

      inner.notifications.broadcast_reconnect(server);
      Ok(())
    },
    _ => Err(Error::new(ErrorKind::Config, "Expected sentinel connections.")),
  }
}
