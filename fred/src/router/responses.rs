#[cfg(feature = "i-tracking")]
use crate::types::client::Invalidation;
use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::{types::Server, utils as protocol_utils, utils::pretty_error},
  runtime::RefCount,
  trace,
  types::{ClientState, Key, KeyspaceEvent, Message, Value},
  utils,
};
use redis_protocol::{
  resp3::types::{BytesFrame as Resp3Frame, FrameKind, Resp3Frame as _Resp3Frame},
  types::PUBSUB_PUSH_PREFIX,
};
use std::str;

const KEYSPACE_PREFIX: &str = "__keyspace@";
const KEYEVENT_PREFIX: &str = "__keyevent@";
#[cfg(feature = "i-tracking")]
const INVALIDATION_CHANNEL: &str = "__redis__:invalidate";

fn parse_keyspace_notification(channel: &str, message: &Value) -> Option<KeyspaceEvent> {
  if channel.starts_with(KEYEVENT_PREFIX) {
    let parts: Vec<&str> = channel.splitn(2, '@').collect();
    if parts.len() < 2 {
      return None;
    }

    let suffix: Vec<&str> = parts[1].splitn(2, ':').collect();
    if suffix.len() < 2 {
      return None;
    }

    let db = suffix[0].replace("__", "").parse::<u8>().ok()?;
    let operation = suffix[1].to_owned();
    let key: Key = message.clone().try_into().ok()?;

    Some(KeyspaceEvent { db, key, operation })
  } else if channel.starts_with(KEYSPACE_PREFIX) {
    let parts: Vec<&str> = channel.splitn(2, '@').collect();
    if parts.len() < 2 {
      return None;
    }

    let suffix: Vec<&str> = parts[1].splitn(2, ':').collect();
    if suffix.len() < 2 {
      return None;
    }

    let db = suffix[0].replace("__", "").parse::<u8>().ok()?;
    let key: Key = suffix[1].to_owned().into();
    let operation = message.as_string()?;

    Some(KeyspaceEvent { db, key, operation })
  } else {
    None
  }
}

#[cfg(feature = "i-tracking")]
fn broadcast_pubsub_invalidation(inner: &RefCount<ClientInner>, message: Message, server: &Server) {
  if let Some(invalidation) = Invalidation::from_message(message, server) {
    inner.notifications.broadcast_invalidation(invalidation);
  } else {
    _debug!(
      inner,
      "Dropping pubsub message on invalidation channel that cannot be parsed as an invalidation message."
    );
  }
}

#[cfg(not(feature = "i-tracking"))]
fn broadcast_pubsub_invalidation(_: &RefCount<ClientInner>, _: Message, _: &Server) {}

#[cfg(feature = "i-tracking")]
fn is_pubsub_invalidation(message: &Message) -> bool {
  message.channel == INVALIDATION_CHANNEL
}

#[cfg(not(feature = "i-tracking"))]
fn is_pubsub_invalidation(_: &Message) -> bool {
  false
}

#[cfg(feature = "i-tracking")]
fn broadcast_resp3_invalidation(inner: &RefCount<ClientInner>, server: &Server, frame: Resp3Frame) {
  if let Resp3Frame::Push { mut data, .. } = frame {
    if data.len() != 2 {
      return;
    }

    // RESP3 example: Push { data: [BlobString { data: b"invalidate", attributes: None }, Array { data:
    //                [BlobString { data: b"foo", attributes: None }], attributes: None }], attributes: None }
    if let Resp3Frame::Array { data, .. } = data[1].take() {
      inner.notifications.broadcast_invalidation(Invalidation {
        keys:   data
          .into_iter()
          .filter_map(|f| f.as_bytes().map(|b| b.into()))
          .collect(),
        server: server.clone(),
      })
    }
  }
}

#[cfg(not(feature = "i-tracking"))]
fn broadcast_resp3_invalidation(_: &RefCount<ClientInner>, _: &Server, _: Resp3Frame) {}

#[cfg(feature = "i-tracking")]
fn is_resp3_invalidation(frame: &Resp3Frame) -> bool {
  // RESP3 example: Push { data: [BlobString { data: b"invalidate", attributes: None }, Array { data:
  //                [BlobString { data: b"foo", attributes: None }], attributes: None }], attributes: None }
  if let Resp3Frame::Push { ref data, .. } = frame {
    data
      .first()
      .and_then(|f| f.as_str())
      .map(|s| s == "invalidate")
      .unwrap_or(false)
  } else {
    false
  }
}

// `SSUBSCRIBE` is intentionally not included so that we can handle MOVED errors. this works as long as we never
// pipeline ssubscribe calls.
fn is_subscribe_prefix(s: &str) -> bool {
  s == "subscribe" || s == "psubscribe"
}

fn is_unsubscribe_prefix(s: &str) -> bool {
  s == "unsubscribe" || s == "punsubscribe" || s == "sunsubscribe"
}

/// Whether the response frame represents a response to any of the subscription interface commands.
fn is_subscription_response(frame: &Resp3Frame) -> bool {
  match frame {
    Resp3Frame::Array { ref data, .. } | Resp3Frame::Push { ref data, .. } => {
      if data.len() >= 3 && data.len() <= 4 {
        // check for ["pubsub", "punsubscribe"|"sunsubscribe", ..] or ["punsubscribe"|"sunsubscribe", ..]
        (data[0].as_str().map(|s| s == PUBSUB_PUSH_PREFIX).unwrap_or(false)
          && data[1]
            .as_str()
            .map(|s| is_subscribe_prefix(s) || is_unsubscribe_prefix(s))
            .unwrap_or(false))
          || (data[0]
            .as_str()
            .map(|s| is_subscribe_prefix(s) || is_unsubscribe_prefix(s))
            .unwrap_or(false))
      } else {
        false
      }
    },
    _ => false,
  }
}

#[cfg(not(feature = "i-tracking"))]
fn is_resp3_invalidation(_: &Resp3Frame) -> bool {
  false
}

/// Check if the frame is part of a pubsub message, and if so route it to any listeners.
///
/// If not then return it to the caller for further processing.
pub fn check_pubsub_message(inner: &RefCount<ClientInner>, server: &Server, frame: Resp3Frame) -> Option<Resp3Frame> {
  if is_subscription_response(&frame) {
    _debug!(inner, "Dropping unused subscription response.");
    return None;
  }
  if is_resp3_invalidation(&frame) {
    broadcast_resp3_invalidation(inner, server, frame);
    return None;
  }

  let is_pubsub =
    frame.is_normal_pubsub_message() || frame.is_pattern_pubsub_message() || frame.is_shard_pubsub_message();
  if !is_pubsub {
    return Some(frame);
  }

  let span = trace::create_pubsub_span(inner, &frame);
  _trace!(inner, "Processing pubsub message from {}.", server);
  let parsed_frame = if let Some(ref span) = span {
    #[allow(clippy::let_unit_value)]
    let _guard = span.enter();
    protocol_utils::frame_to_pubsub(server, frame)
  } else {
    protocol_utils::frame_to_pubsub(server, frame)
  };

  let message = match parsed_frame {
    Ok(data) => data,
    Err(err) => {
      _warn!(inner, "Invalid message on pubsub interface: {:?}", err);
      return None;
    },
  };
  if let Some(ref span) = span {
    span.record("channel", &*message.channel);
  }

  if is_pubsub_invalidation(&message) {
    broadcast_pubsub_invalidation(inner, message, server);
  } else if let Some(event) = parse_keyspace_notification(&message.channel, &message.value) {
    inner.notifications.broadcast_keyspace(event);
  } else {
    inner.notifications.broadcast_pubsub(message);
  }

  None
}

/// Parse the response frame to see if it's an auth error.
fn parse_auth_error(frame: &Resp3Frame) -> Option<Error> {
  if matches!(frame.kind(), FrameKind::SimpleError | FrameKind::BlobError) {
    match protocol_utils::frame_to_results(frame.clone()) {
      Ok(_) => None,
      Err(e) => match e.kind() {
        ErrorKind::Auth => Some(e),
        _ => None,
      },
    }
  } else {
    None
  }
}

#[cfg(feature = "custom-reconnect-errors")]
fn check_global_reconnect_errors(
  inner: &RefCount<ClientInner>,
  server: &Server,
  frame: &Resp3Frame,
) -> Option<Error> {
  if let Resp3Frame::SimpleError { ref data, .. } = frame {
    for prefix in inner.connection.reconnect_errors.iter() {
      if data.starts_with(prefix.to_str()) {
        _warn!(inner, "Found reconnection error: {}", data);
        let error = protocol_utils::pretty_error(data);
        inner.notifications.broadcast_error(error.clone(), Some(server.clone()));
        return Some(error);
      }
    }

    None
  } else {
    None
  }
}

#[cfg(not(feature = "custom-reconnect-errors"))]
fn check_global_reconnect_errors(_: &RefCount<ClientInner>, _: &Server, _: &Resp3Frame) -> Option<Error> {
  None
}

fn is_clusterdown_error(frame: &Resp3Frame) -> Option<&str> {
  match frame {
    Resp3Frame::SimpleError { data, .. } => {
      if data.trim().starts_with("CLUSTERDOWN") {
        Some(data)
      } else {
        None
      }
    },
    Resp3Frame::BlobError { data, .. } => {
      let parsed = match str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return None,
      };

      if parsed.trim().starts_with("CLUSTERDOWN") {
        Some(parsed)
      } else {
        None
      }
    },
    _ => None,
  }
}

/// Check for fatal errors configured by the caller to initiate a reconnection process.
pub fn check_fatal_errors(inner: &RefCount<ClientInner>, server: &Server, frame: &Resp3Frame) -> Option<Error> {
  if inner.connection.reconnect_on_auth_error {
    if let Some(auth_error) = parse_auth_error(frame) {
      return Some(auth_error);
    }
  }
  if let Some(error) = is_clusterdown_error(frame) {
    return Some(pretty_error(error));
  }

  check_global_reconnect_errors(inner, server, frame)
}

/// Check for special errors, pubsub messages, or other special response frames.
///
/// The frame is returned to the caller for further processing if necessary.
pub fn preprocess_frame(
  inner: &RefCount<ClientInner>,
  server: &Server,
  frame: Resp3Frame,
) -> Result<Option<Resp3Frame>, Error> {
  if let Some(error) = check_fatal_errors(inner, server, &frame) {
    Err(error)
  } else {
    Ok(check_pubsub_message(inner, server, frame))
  }
}

/// Handle an error in the reader task that should end the connection.
pub fn broadcast_reader_error(inner: &RefCount<ClientInner>, server: &Server, error: Option<Error>) {
  _warn!(inner, "Broadcasting error {:?} from {}", error, server);

  if utils::read_locked(&inner.state) != ClientState::Disconnecting {
    inner
      .notifications
      .broadcast_error(error.unwrap_or(Error::new_canceled()), Some(server.clone()));
  }
}

#[cfg(feature = "replicas")]
pub fn broadcast_replica_error(inner: &RefCount<ClientInner>, server: &Server, error: Option<Error>) {
  _warn!(inner, "Broadcasting replica error {:?} from {}", error, server);

  if utils::read_locked(&inner.state) != ClientState::Disconnecting {
    inner
      .notifications
      .broadcast_error(error.unwrap_or(Error::new_canceled()), Some(server.clone()));
  }
}
