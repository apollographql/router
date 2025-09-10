use crate::{
  error::{Error, ErrorKind},
  interfaces::ClientLike,
  modules::inner::{ClientInner, CommandSender},
  prelude::{Blocking, Server},
  protocol::{
    command::{Command, CommandKind},
    responders::ResponseKind,
    utils as protocol_utils,
  },
  runtime::{
    broadcast_channel,
    channel,
    oneshot_channel,
    AtomicBool,
    AtomicUsize,
    BroadcastSender,
    RefCount,
    RefSwap,
    RwLock,
  },
  types::{ClientUnblockFlag, *},
};
use bytes::Bytes;
use bytes_utils::Str;
use float_cmp::approx_eq;
use futures::{Future, TryFutureExt};
use rand::{self, distributions::Alphanumeric, Rng};
use redis_protocol::resp3::types::BytesFrame as Resp3Frame;
use std::{collections::HashMap, convert::TryInto, f64, sync::atomic::Ordering, time::Duration};
use url::Url;
use urlencoding::decode as percent_decode;

#[cfg(any(
  feature = "enable-native-tls",
  feature = "enable-rustls",
  feature = "enable-rustls-ring"
))]
use crate::protocol::tls::{TlsConfig, TlsConnector};
#[cfg(feature = "transactions")]
use crate::runtime::Mutex;
#[cfg(any(feature = "full-tracing", feature = "partial-tracing"))]
use crate::trace;
#[cfg(feature = "i-scripts")]
use crate::types::scripts::{Function, FunctionFlag};
#[cfg(feature = "i-sorted-sets")]
use crate::types::sorted_sets::ZRangeKind;
#[cfg(feature = "transactions")]
use std::mem;
#[cfg(feature = "unix-sockets")]
use std::path::{Path, PathBuf};
#[cfg(any(feature = "full-tracing", feature = "partial-tracing"))]
use tracing_futures::Instrument;

const REDIS_TLS_SCHEME: &str = "rediss";
const VALKEY_TLS_SCHEME: &str = "valkeys";
const CLUSTER_SCHEME_SUFFIX: &str = "-cluster";
const SENTINEL_SCHEME_SUFFIX: &str = "-sentinel";
const UNIX_SCHEME_SUFFIX: &str = "+unix";
const SENTINEL_NAME_QUERY: &str = "sentinelServiceName";
const CLUSTER_NODE_QUERY: &str = "node";
#[cfg(feature = "sentinel-auth")]
const SENTINEL_USERNAME_QUERY: &str = "sentinelUsername";
#[cfg(feature = "sentinel-auth")]
const SENTINEL_PASSWORD_QUERY: &str = "sentinelPassword";

/// Create a `Str` from a static str slice without copying.
pub const fn static_str(s: &'static str) -> Str {
  // it's already parsed as a string
  unsafe { Str::from_inner_unchecked(Bytes::from_static(s.as_bytes())) }
}

/// Create a `Bytes` from static bytes without copying.
pub fn static_bytes(b: &'static [u8]) -> Bytes {
  Bytes::from_static(b)
}

pub fn f64_eq(lhs: f64, rhs: f64) -> bool {
  approx_eq!(f64, lhs, rhs, ulps = 2)
}

#[cfg(feature = "i-geo")]
pub fn f64_opt_eq(lhs: &Option<f64>, rhs: &Option<f64>) -> bool {
  match *lhs {
    Some(lhs) => match *rhs {
      Some(rhs) => f64_eq(lhs, rhs),
      None => false,
    },
    None => rhs.is_none(),
  }
}

/// Convert a string to an `f64`, supporting "+inf" and "-inf".
pub fn string_to_f64(s: &str) -> Result<f64, Error> {
  // this is changing in newer versions of redis to lose the "+" prefix
  if s == "+inf" || s == "inf" {
    Ok(f64::INFINITY)
  } else if s == "-inf" {
    Ok(f64::NEG_INFINITY)
  } else {
    s.parse::<f64>().map_err(|_| {
      Error::new(
        ErrorKind::Unknown,
        format!("Could not convert {} to floating point value.", s),
      )
    })
  }
}

/// Convert an `f64` to a string, supporting "+inf" and "-inf".
pub fn f64_to_string(d: f64) -> Result<Value, Error> {
  if d.is_infinite() && d.is_sign_negative() {
    Ok(Value::from_static_str("-inf"))
  } else if d.is_infinite() {
    Ok(Value::from_static_str("+inf"))
  } else if d.is_nan() {
    Err(Error::new(
      ErrorKind::InvalidArgument,
      "Cannot convert NaN to redis value.",
    ))
  } else {
    Ok(d.to_string().into())
  }
}

#[cfg(feature = "i-sorted-sets")]
pub fn f64_to_zrange_bound(d: f64, kind: &ZRangeKind) -> Result<String, Error> {
  if d.is_infinite() && d.is_sign_negative() {
    Ok("-inf".into())
  } else if d.is_infinite() {
    Ok("+inf".into())
  } else if d.is_nan() {
    Err(Error::new(
      ErrorKind::InvalidArgument,
      "Cannot convert NaN to redis value.",
    ))
  } else {
    Ok(match kind {
      ZRangeKind::Inclusive => d.to_string(),
      ZRangeKind::Exclusive => format!("({}", d),
    })
  }
}

pub fn incr_with_max(curr: u32, max: u32) -> Option<u32> {
  if max != 0 && curr >= max {
    None
  } else {
    Some(curr.saturating_add(1))
  }
}

pub fn random_string(len: usize) -> String {
  rand::thread_rng()
    .sample_iter(&Alphanumeric)
    .take(len)
    .map(char::from)
    .collect()
}

#[cfg(feature = "i-memory")]
pub fn convert_or_default<R>(value: Value) -> R
where
  R: FromValue + Default,
{
  value.convert().ok().unwrap_or_default()
}

#[cfg(feature = "transactions")]
pub fn random_u64(max: u64) -> u64 {
  rand::thread_rng().gen_range(0 .. max)
}

pub fn read_bool_atomic(val: &AtomicBool) -> bool {
  val.load(Ordering::Acquire)
}

pub fn set_bool_atomic(val: &AtomicBool, new: bool) -> bool {
  val.swap(new, Ordering::SeqCst)
}

pub fn decr_atomic(size: &AtomicUsize) -> usize {
  size.fetch_sub(1, Ordering::AcqRel).saturating_sub(1)
}

pub fn incr_atomic(size: &AtomicUsize) -> usize {
  size.fetch_add(1, Ordering::AcqRel).saturating_add(1)
}

pub fn read_atomic(size: &AtomicUsize) -> usize {
  size.load(Ordering::Acquire)
}

pub fn set_atomic(size: &AtomicUsize, val: usize) -> usize {
  size.swap(val, Ordering::SeqCst)
}

pub fn read_locked<T: Clone>(locked: &RwLock<T>) -> T {
  locked.read().clone()
}

#[cfg(feature = "transactions")]
pub fn read_mutex<T: Clone>(locked: &Mutex<T>) -> T {
  locked.lock().clone()
}

#[cfg(feature = "transactions")]
pub fn set_mutex<T>(locked: &Mutex<T>, value: T) -> T {
  mem::replace(&mut *locked.lock(), value)
}

#[cfg(feature = "unix-sockets")]
pub fn path_to_string(path: &Path) -> String {
  path.as_os_str().to_string_lossy().to_string()
}

#[cfg(feature = "i-sorted-sets")]
pub fn check_lex_str(val: String, kind: &ZRangeKind) -> String {
  let formatted = val.starts_with('(') || val.starts_with('[') || val == "+" || val == "-";

  if formatted {
    val
  } else if *kind == ZRangeKind::Exclusive {
    format!("({}", val)
  } else {
    format!("[{}", val)
  }
}

/// Parse the response from `FUNCTION LIST`.
#[cfg(feature = "i-scripts")]
fn parse_functions(value: &Value) -> Result<Vec<Function>, Error> {
  if let Value::Array(functions) = value {
    let mut out = Vec::with_capacity(functions.len());
    for function_block in functions.iter() {
      let functions: HashMap<Str, Value> = function_block.clone().convert()?;
      let name = match functions.get("name").and_then(|n| n.as_bytes_str()) {
        Some(name) => name,
        None => return Err(Error::new_parse("Missing function name.")),
      };
      let flags: Vec<FunctionFlag> = functions
        .get("flags")
        .and_then(|f| {
          f.clone()
            .into_array()
            .into_iter()
            .map(|v| FunctionFlag::from_str(v.as_str().unwrap_or_default().as_ref()))
            .collect()
        })
        .unwrap_or_default();

      out.push(Function { name, flags })
    }

    Ok(out)
  } else {
    Err(Error::new_parse("Invalid functions block."))
  }
}

/// Check and parse the response to `FUNCTION LIST`.
#[cfg(feature = "i-scripts")]
pub fn value_to_functions(value: &Value, name: &str) -> Result<Vec<Function>, Error> {
  if let Value::Array(ref libraries) = value {
    for library in libraries.iter() {
      let properties: HashMap<Str, Value> = library.clone().convert()?;
      let should_parse = properties
        .get("library_name")
        .and_then(|v| v.as_str())
        .map(|s| s == name)
        .unwrap_or(false);

      if should_parse {
        if let Some(functions) = properties.get("functions") {
          return parse_functions(functions);
        }
      }
    }

    Err(Error::new_parse(format!("Missing library '{}'", name)))
  } else {
    Err(Error::new_parse("Expected array."))
  }
}

pub async fn timeout<T, Fut, E>(ft: Fut, timeout: Duration) -> Result<T, Error>
where
  E: Into<Error>,
  Fut: Future<Output = Result<T, E>>,
{
  if !timeout.is_zero() {
    tokio::time::timeout(timeout, ft)
      .await
      .map_err(|_| Error::new(ErrorKind::Timeout, "Request timed out."))
      .and_then(|r| r.map_err(|e| e.into()))
  } else {
    ft.await.map_err(|e| e.into())
  }
}

/// Disconnect any state shared with the last router task spawned by the client.
pub fn reset_router_task(inner: &RefCount<ClientInner>) {
  let _guard = inner._lock.lock();

  if !inner.has_command_rx() {
    _trace!(inner, "Resetting command channel before connecting.");
    // another connection task is running. this will let the command channel drain, then it'll drop everything on
    // the old connection/router interface.
    let (tx, rx) = channel(inner.connection.max_command_buffer_len);

    let old_command_tx = inner.swap_command_tx(tx);
    inner.store_command_rx(rx, true);
    close_router_channel(inner, old_command_tx);
  }
}

/// Whether the router should check and interrupt the blocked command.
fn should_enforce_blocking_policy(inner: &RefCount<ClientInner>, command: &Command) -> bool {
  if command.kind.closes_connection() {
    return false;
  }
  if matches!(inner.config.blocking, Blocking::Error | Blocking::Interrupt) {
    inner.backchannel.is_blocked()
  } else {
    false
  }
}

/// Interrupt the currently blocked connection (if found) with the provided flag.
pub async fn interrupt_blocked_connection(
  inner: &RefCount<ClientInner>,
  flag: ClientUnblockFlag,
) -> Result<(), Error> {
  let connection_id = {
    let server = match inner.backchannel.blocked_server() {
      Some(server) => server,
      None => return Err(Error::new(ErrorKind::Unknown, "Connection is not blocked.")),
    };
    let id = match inner.backchannel.connection_id(&server) {
      Some(id) => id,
      None => return Err(Error::new(ErrorKind::Unknown, "Failed to read connection ID.")),
    };

    _debug!(inner, "Sending CLIENT UNBLOCK to {}, ID: {}", server, id);
    id
  };

  let command = Command::new(CommandKind::ClientUnblock, vec![
    connection_id.into(),
    flag.to_str().into(),
  ]);
  let frame = backchannel_request_response(inner, command, true).await?;
  protocol_utils::frame_to_results(frame).map(|_| ())
}

/// Check the status of the connection (usually before sending a command) to determine whether the connection should
/// be unblocked automatically.
async fn check_blocking_policy(inner: &RefCount<ClientInner>, command: &Command) -> Result<(), Error> {
  if should_enforce_blocking_policy(inner, command) {
    _debug!(
      inner,
      "Checking to enforce blocking policy for {}",
      command.kind.to_str_debug()
    );

    if inner.config.blocking == Blocking::Error {
      return Err(Error::new(
        ErrorKind::InvalidCommand,
        "Error sending command while connection is blocked.",
      ));
    } else if inner.config.blocking == Blocking::Interrupt {
      if let Err(e) = interrupt_blocked_connection(inner, ClientUnblockFlag::Error).await {
        _error!(inner, "Failed to interrupt blocked connection: {:?}", e);
      }
    }
  }

  Ok(())
}

/// Prepare the command options, returning the timeout duration to apply.
pub fn prepare_command<C: ClientLike>(client: &C, command: &mut Command) -> Duration {
  client.change_command(command);
  command.inherit_options(client.inner());
  command
    .timeout_dur
    .unwrap_or_else(|| client.inner().default_command_timeout())
}

/// Send a command to the server using the default response handler.
pub async fn basic_request_response<C, F, R>(client: &C, func: F) -> Result<Resp3Frame, Error>
where
  C: ClientLike,
  R: Into<Command>,
  F: FnOnce() -> Result<R, Error>,
{
  let inner = client.inner();
  let mut command: Command = func()?.into();
  let (tx, rx) = oneshot_channel();
  command.response = ResponseKind::Respond(Some(tx));

  let timed_out = command.timed_out.clone();
  let timeout_dur = prepare_command(client, &mut command);
  check_blocking_policy(inner, &command).await?;
  client.send_command(command)?;

  if timeout_dur.is_zero() {
    rx.map_err(move |error| {
      set_bool_atomic(&timed_out, true);
      Error::from(error)
    })
    .await?
  } else {
    timeout(rx, timeout_dur)
      .and_then(|r| async { r })
      .map_err(move |error| {
        set_bool_atomic(&timed_out, true);
        error
      })
      .await
  }
}

/// Send a command to the server, with tracing.
#[cfg(any(feature = "full-tracing", feature = "partial-tracing"))]
#[allow(clippy::needless_borrows_for_generic_args)]
// despite what clippy says, this^ actually matters for tracing `record` calls (at least it seems where `V: Copy`)
pub async fn request_response<C, F, R>(client: &C, func: F) -> Result<Resp3Frame, Error>
where
  C: ClientLike,
  R: Into<Command>,
  F: FnOnce() -> Result<R, Error>,
{
  let inner = client.inner();
  if !inner.should_trace() {
    return basic_request_response(client, func).await;
  }

  let cmd_span = trace::create_command_span(inner);
  let end_cmd_span = cmd_span.clone();

  let (mut command, rx, req_size) = {
    let args_span = trace::create_args_span(cmd_span.id(), inner);
    #[allow(clippy::let_unit_value)]
    let _guard = args_span.enter();
    let (tx, rx) = oneshot_channel();

    let mut command: Command = func()?.into();
    command.response = ResponseKind::Respond(Some(tx));

    let req_size = protocol_utils::args_size(command.args());
    args_span.record("num_args", &command.args().len());
    (command, rx, req_size)
  };
  cmd_span.record("cmd.name", &command.kind.to_str_debug());
  cmd_span.record("cmd.req", &req_size);

  let queued_span = trace::create_queued_span(cmd_span.id(), inner);
  let timed_out = command.timed_out.clone();
  _trace!(
    inner,
    "Setting command trace ID: {:?} for {} ({})",
    cmd_span.id(),
    command.kind.to_str_debug(),
    command.debug_id()
  );
  command.traces.cmd = Some(cmd_span.clone());
  command.traces.queued = Some(queued_span);

  let timeout_dur = prepare_command(client, &mut command);
  check_blocking_policy(inner, &command).await?;
  client.send_command(command)?;

  let ft = async { rx.instrument(cmd_span).await.map_err(|e| e.into()).and_then(|r| r) };
  let result = if timeout_dur.is_zero() {
    ft.await
  } else {
    timeout(ft, timeout_dur).await
  };

  if let Ok(ref frame) = result {
    trace::record_response_size(&end_cmd_span, frame);
  } else {
    set_bool_atomic(&timed_out, true);
  }
  result
}

#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
pub async fn request_response<C, F, R>(client: &C, func: F) -> Result<Resp3Frame, Error>
where
  C: ClientLike,
  R: Into<Command>,
  F: FnOnce() -> Result<R, Error>,
{
  basic_request_response(client, func).await
}

/// Send a command on the backchannel connection.
///
/// A new connection may be created.
pub async fn backchannel_request_response(
  inner: &RefCount<ClientInner>,
  command: Command,
  use_blocked: bool,
) -> Result<Resp3Frame, Error> {
  let server = inner.backchannel.find_server(inner, &command, use_blocked).await?;
  inner.backchannel.request_response(inner, &server, command).await
}

/// Check for a scan pattern without a hash tag, or with a wildcard in the hash tag.
///
/// These patterns will result in scanning a random node if used against a clustered redis.
pub fn clustered_scan_pattern_has_hash_tag(inner: &RefCount<ClientInner>, pattern: &str) -> bool {
  let (mut i, mut j, mut has_wildcard) = (None, None, false);
  for (idx, c) in pattern.chars().enumerate() {
    if c == '{' && i.is_none() {
      i = Some(idx);
    }
    if c == '}' && j.is_none() && i.is_some() {
      j = Some(idx);
      break;
    }
    if c == '*' && i.is_some() {
      has_wildcard = true;
    }
  }

  if i.is_none() || j.is_none() {
    return false;
  }

  if has_wildcard {
    _warn!(
      inner,
      "Found wildcard in scan pattern hash tag. You may not be scanning the correct node."
    );
  }

  true
}

/// A generic TryInto wrapper to work with the Infallible error type in the blanket From implementation.
pub fn try_into<S, D>(val: S) -> Result<D, Error>
where
  S: TryInto<D>,
  S::Error: Into<Error>,
{
  val.try_into().map_err(|e| e.into())
}

pub fn try_into_vec<S>(values: Vec<S>) -> Result<Vec<Value>, Error>
where
  S: TryInto<Value>,
  S::Error: Into<Error>,
{
  let mut out = Vec::with_capacity(values.len());
  for value in values.into_iter() {
    out.push(try_into(value)?);
  }

  Ok(out)
}

pub fn add_jitter(delay: u64, jitter: u32) -> u64 {
  if jitter == 0 {
    delay
  } else {
    delay.saturating_add(rand::thread_rng().gen_range(0 .. jitter as u64))
  }
}

pub fn into_map<I, K, V>(mut iter: I) -> Result<HashMap<Key, Value>, Error>
where
  I: Iterator<Item = (K, V)>,
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  let (lower, upper) = iter.size_hint();
  let capacity = if let Some(upper) = upper { upper } else { lower };
  let mut out = HashMap::with_capacity(capacity);

  while let Some((key, value)) = iter.next() {
    out.insert(to!(key)?, to!(value)?);
  }
  Ok(out)
}

pub fn flatten_nested_array_values(value: Value, depth: usize) -> Value {
  if depth == 0 {
    return value;
  }

  match value {
    Value::Array(values) => {
      let inner_size = values.iter().fold(0, |s, v| s + v.array_len().unwrap_or(1));
      let mut out = Vec::with_capacity(inner_size);

      for value in values.into_iter() {
        match value {
          Value::Array(inner) => {
            for value in inner.into_iter() {
              out.push(flatten_nested_array_values(value, depth - 1));
            }
          },
          _ => out.push(value),
        }
      }
      Value::Array(out)
    },
    Value::Map(values) => {
      let mut out = HashMap::with_capacity(values.len());

      for (key, value) in values.inner().into_iter() {
        let value = if value.is_array() {
          flatten_nested_array_values(value, depth - 1)
        } else {
          value
        };

        out.insert(key, value);
      }
      Value::Map(Map { inner: out })
    },
    _ => value,
  }
}

pub fn is_maybe_array_map(arr: &[Value]) -> bool {
  if !arr.is_empty() && arr.len() % 2 == 0 {
    arr.chunks(2).all(|chunk| !chunk[0].is_aggregate_type())
  } else {
    false
  }
}

#[cfg(any(
  feature = "enable-native-tls",
  feature = "enable-rustls",
  feature = "enable-rustls-ring"
))]
pub fn check_tls_features() {}

#[cfg(not(any(
  feature = "enable-native-tls",
  feature = "enable-rustls",
  feature = "enable-rustls-ring"
)))]
pub fn check_tls_features() {
  warn!("TLS features are not enabled, but a TLS feature may have been used.");
}

#[cfg(all(
  feature = "enable-native-tls",
  not(any(feature = "enable-rustls", feature = "enable-rustls-ring"))
))]
pub fn tls_config_from_url(tls: bool) -> Result<Option<TlsConfig>, Error> {
  if tls {
    TlsConnector::default_native_tls().map(|c| Some(c.into()))
  } else {
    Ok(None)
  }
}

#[cfg(all(
  any(feature = "enable-rustls", feature = "enable-rustls-ring"),
  not(feature = "enable-native-tls")
))]
pub fn tls_config_from_url(tls: bool) -> Result<Option<TlsConfig>, Error> {
  if tls {
    TlsConnector::default_rustls().map(|c| Some(c.into()))
  } else {
    Ok(None)
  }
}

#[cfg(all(
  feature = "enable-native-tls",
  any(feature = "enable-rustls", feature = "enable-rustls-ring")
))]
pub fn tls_config_from_url(tls: bool) -> Result<Option<TlsConfig>, Error> {
  // default to native-tls when both are enabled
  if tls {
    TlsConnector::default_native_tls().map(|c| Some(c.into()))
  } else {
    Ok(None)
  }
}

pub fn swap_new_broadcast_channel<T: Clone>(old: &RefSwap<RefCount<BroadcastSender<T>>>, capacity: usize) {
  let new = broadcast_channel(capacity).0;
  old.swap(RefCount::new(new));
}

pub fn url_uses_tls(url: &Url) -> bool {
  let scheme = url.scheme();
  scheme.starts_with(REDIS_TLS_SCHEME) || scheme.starts_with(VALKEY_TLS_SCHEME)
}

pub fn url_is_clustered(url: &Url) -> bool {
  url.scheme().ends_with(CLUSTER_SCHEME_SUFFIX)
}

pub fn url_is_sentinel(url: &Url) -> bool {
  url.scheme().ends_with(SENTINEL_SCHEME_SUFFIX)
}

pub fn url_is_unix_socket(url: &Url) -> bool {
  url.scheme().ends_with(UNIX_SCHEME_SUFFIX)
}

pub fn parse_url(url: &str, default_port: Option<u16>) -> Result<(Url, String, u16, bool), Error> {
  let url = Url::parse(url)?;
  let host = if let Some(host) = url.host_str() {
    host.to_owned()
  } else {
    return Err(Error::new(ErrorKind::Config, "Invalid or missing host."));
  };
  let port = if let Some(port) = url.port().or(default_port) {
    port
  } else {
    return Err(Error::new(ErrorKind::Config, "Invalid or missing port."));
  };

  let tls = url_uses_tls(&url);
  if tls {
    check_tls_features();
  }

  Ok((url, host, port, tls))
}

#[cfg(feature = "unix-sockets")]
pub fn parse_unix_url(url: &str) -> Result<(Url, PathBuf), Error> {
  let url = Url::parse(url)?;
  let path: PathBuf = url.path().into();
  Ok((url, path))
}

pub fn parse_url_db(url: &Url) -> Result<Option<u8>, Error> {
  let parts: Vec<&str> = if let Some(parts) = url.path_segments() {
    parts.collect()
  } else {
    return Ok(None);
  };

  if parts.len() > 1 {
    return Err(Error::new(ErrorKind::Config, "Invalid database path."));
  } else if parts.is_empty() {
    return Ok(None);
  }
  // handle empty paths with a / prefix
  if parts[0].trim() == "" {
    return Ok(None);
  }

  Ok(Some(parts[0].parse()?))
}

pub fn parse_url_credentials(url: &Url) -> Result<(Option<String>, Option<String>), Error> {
  let username = if url.username().is_empty() {
    None
  } else {
    let username = percent_decode(url.username())?;
    Some(username.into_owned())
  };
  let password = percent_decode(url.password().unwrap_or_default())?;
  let password = if password.is_empty() {
    None
  } else {
    Some(password.into_owned())
  };

  Ok((username, password))
}

pub fn parse_url_other_nodes(url: &Url) -> Result<Vec<Server>, Error> {
  let mut out = Vec::new();

  for (key, value) in url.query_pairs().into_iter() {
    if key == CLUSTER_NODE_QUERY {
      let parts: Vec<&str> = value.split(':').collect();
      if parts.len() != 2 {
        return Err(Error::new(
          ErrorKind::Config,
          format!("Invalid host:port for cluster node: {}", value),
        ));
      }

      let host = parts[0].to_owned();
      let port = parts[1].parse::<u16>()?;
      out.push(Server::new(host, port));
    }
  }

  Ok(out)
}

pub fn parse_url_sentinel_service_name(url: &Url) -> Result<String, Error> {
  for (key, value) in url.query_pairs().into_iter() {
    if key == SENTINEL_NAME_QUERY {
      return Ok(value.to_string());
    }
  }

  Err(Error::new(
    ErrorKind::Config,
    "Invalid or missing sentinel service name query parameter.",
  ))
}

#[cfg(feature = "sentinel-auth")]
pub fn parse_url_sentinel_username(url: &Url) -> Option<String> {
  url.query_pairs().find_map(|(key, value)| {
    if key == SENTINEL_USERNAME_QUERY {
      Some(value.to_string())
    } else {
      None
    }
  })
}

#[cfg(feature = "sentinel-auth")]
pub fn parse_url_sentinel_password(url: &Url) -> Option<String> {
  url.query_pairs().find_map(|(key, value)| {
    if key == SENTINEL_PASSWORD_QUERY {
      Some(value.to_string())
    } else {
      None
    }
  })
}

/// Send QUIT to the servers and clean up the old router task's state.
fn close_router_channel(inner: &RefCount<ClientInner>, command_tx: RefCount<CommandSender>) {
  inner.notifications.broadcast_close();
  inner.reset_server_state();

  let command = Command::new(CommandKind::Quit, vec![]);
  inner.counters.incr_cmd_buffer_len();
  if let Err(_) = command_tx.try_send(command.into()) {
    inner.counters.decr_cmd_buffer_len();
    _warn!(inner, "Failed to send QUIT when dropping old command channel.");
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{error::Error, types::Value};
  use std::{convert::TryInto, fmt::Debug};

  fn m<V>(v: V) -> Value
  where
    V: TryInto<Value> + Debug,
    V::Error: Into<Error> + Debug,
  {
    v.try_into().unwrap()
  }

  fn a(v: Vec<Value>) -> Value {
    Value::Array(v)
  }

  #[test]
  fn should_not_panic_with_zero_jitter() {
    assert_eq!(add_jitter(10, 0), 10);
  }

  #[test]
  fn should_flatten_xread_example() {
    // 127.0.0.1:6379> xread count 2 streams foo bar 1643479648480-0 1643479834990-0
    // 1) 1) "foo"
    //    2) 1) 1) "1643479650336-0"
    //          2) 1) "count"
    //             2) "3"
    // 2) 1) "bar"
    //    2) 1) 1) "1643479837746-0"
    //          2) 1) "count"
    //             2) "5"
    //       2) 1) "1643479925582-0"
    //          2) 1) "count"
    //             2) "6"
    let actual: Value = vec![
      a(vec![
        m("foo"),
        a(vec![a(vec![m("1643479650336-0"), a(vec![m("count"), m(3)])])]),
      ]),
      a(vec![
        m("bar"),
        a(vec![
          a(vec![m("1643479837746-0"), a(vec![m("count"), m(5)])]),
          a(vec![m("1643479925582-0"), a(vec![m("count"), m(6)])]),
        ]),
      ]),
    ]
    .into_iter()
    .collect();

    // flatten the top level nested array into something that can be cast to a map
    let expected: Value = vec![
      m("foo"),
      a(vec![a(vec![m("1643479650336-0"), a(vec![m("count"), m(3)])])]),
      m("bar"),
      a(vec![
        a(vec![m("1643479837746-0"), a(vec![m("count"), m(5)])]),
        a(vec![m("1643479925582-0"), a(vec![m("count"), m(6)])]),
      ]),
    ]
    .into_iter()
    .collect();

    assert_eq!(flatten_nested_array_values(actual, 1), expected);
  }

  #[test]
  fn should_parse_url_credentials_no_creds() {
    let url = Url::parse("redis://localhost:6379").unwrap();
    let (username, password) = parse_url_credentials(&url).unwrap();

    assert_eq!(username, None);
    assert_eq!(password, None);
  }

  #[test]
  fn should_parse_url_credentials_with_creds() {
    let url = Url::parse("redis://default:abc123@localhost:6379").unwrap();
    let (username, password) = parse_url_credentials(&url).unwrap();

    assert_eq!(username.unwrap(), "default");
    assert_eq!(password.unwrap(), "abc123");
  }

  #[test]
  fn should_parse_url_credentials_with_percent_encoded_creds() {
    let url = Url::parse("redis://default:abc%2F123@localhost:6379").unwrap();
    let (username, password) = parse_url_credentials(&url).unwrap();

    assert_eq!(username.unwrap(), "default");
    assert_eq!(password.unwrap(), "abc/123");
  }
}
