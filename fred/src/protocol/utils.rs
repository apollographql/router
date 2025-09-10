use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::{
    codec::Codec,
    command::{ClusterErrorKind, Command, CommandKind},
    connection::OK,
    types::{ProtocolFrame, *},
  },
  runtime::RefCount,
  types::*,
  utils,
};
use bytes::Bytes;
use bytes_utils::Str;
use redis_protocol::{
  resp2::types::{BytesFrame as Resp2Frame, Resp2Frame as _Resp2Frame},
  resp3::types::{BytesFrame as Resp3Frame, Resp3Frame as _Resp3Frame},
  types::{PUBSUB_PUSH_PREFIX, REDIS_CLUSTER_SLOTS},
};
use std::{borrow::Cow, collections::HashMap, convert::TryInto, ops::Deref, str};

#[cfg(any(feature = "i-lists", feature = "i-sorted-sets"))]
use redis_protocol::resp3::types::FrameKind;
#[cfg(feature = "i-hashes")]
use redis_protocol::resp3::types::FrameMap;

static LEGACY_AUTH_ERROR_BODY: &str = "ERR Client sent AUTH, but no password is set";
static ACL_AUTH_ERROR_PREFIX: &str =
  "ERR AUTH <password> called without any password configured for the default user";

pub fn parse_cluster_error(data: &str) -> Result<(ClusterErrorKind, u16, String), Error> {
  let parts: Vec<&str> = data.split(' ').collect();
  if parts.len() == 3 {
    let kind: ClusterErrorKind = parts[0].try_into()?;
    let slot: u16 = parts[1].parse()?;
    let server = parts[2].to_string();

    Ok((kind, slot, server))
  } else {
    Err(Error::new(ErrorKind::Protocol, "Expected cluster error."))
  }
}

pub fn queued_frame() -> Resp3Frame {
  Resp3Frame::SimpleString {
    data:       utils::static_bytes(QUEUED.as_bytes()),
    attributes: None,
  }
}

pub fn is_ok(frame: &Resp3Frame) -> bool {
  match frame {
    Resp3Frame::SimpleString { ref data, .. } => data == OK,
    _ => false,
  }
}

pub fn server_to_parts(server: &str) -> Result<(&str, u16), Error> {
  let parts: Vec<&str> = server.split(':').collect();
  if parts.len() < 2 {
    return Err(Error::new(ErrorKind::IO, "Invalid server."));
  }
  Ok((parts[0], parts[1].parse::<u16>()?))
}

pub fn binary_search(slots: &[SlotRange], slot: u16) -> Option<usize> {
  if slot > REDIS_CLUSTER_SLOTS {
    return None;
  }

  let (mut low, mut high) = (0, slots.len() - 1);
  while low <= high {
    let mid = (low + high) / 2;

    let curr = match slots.get(mid) {
      Some(slot) => slot,
      None => {
        warn!("Failed to find slot range at index {} for hash slot {}", mid, slot);
        return None;
      },
    };

    if slot < curr.start {
      high = mid - 1;
    } else if slot > curr.end {
      low = mid + 1;
    } else {
      return Some(mid);
    }
  }

  None
}

pub fn pretty_error(resp: &str) -> Error {
  let kind = {
    let mut parts = resp.split_whitespace();

    match parts.next().unwrap_or("") {
      "" => ErrorKind::Unknown,
      "ERR" => {
        if resp.contains("instance has cluster support disabled") {
          // Cluster client connecting to non-cluster server.
          // Returning Config to signal no reconnect will help.
          ErrorKind::Config
        } else {
          ErrorKind::Unknown
        }
      },
      "WRONGTYPE" => ErrorKind::InvalidArgument,
      "NOAUTH" | "WRONGPASS" => ErrorKind::Auth,
      "MOVED" | "ASK" | "CLUSTERDOWN" => ErrorKind::Cluster,
      "Invalid" => match parts.next().unwrap_or("") {
        "argument(s)" | "Argument" => ErrorKind::InvalidArgument,
        "command" | "Command" => ErrorKind::InvalidCommand,
        _ => ErrorKind::Unknown,
      },
      _ => ErrorKind::Unknown,
    }
  };

  let details = if resp.is_empty() {
    Cow::Borrowed("No response!")
  } else {
    Cow::Owned(resp.to_owned())
  };
  Error::new(kind, details)
}

/// Parse the frame as a string, without support for error frames.
pub fn frame_into_string(frame: Resp3Frame) -> Result<String, Error> {
  match frame {
    Resp3Frame::SimpleString { data, .. } => Ok(String::from_utf8(data.to_vec())?),
    Resp3Frame::BlobString { data, .. } => Ok(String::from_utf8(data.to_vec())?),
    Resp3Frame::Double { data, .. } => Ok(data.to_string()),
    Resp3Frame::Number { data, .. } => Ok(data.to_string()),
    Resp3Frame::Boolean { data, .. } => Ok(data.to_string()),
    Resp3Frame::VerbatimString { data, .. } => Ok(String::from_utf8(data.to_vec())?),
    Resp3Frame::BigNumber { data, .. } => Ok(String::from_utf8(data.to_vec())?),
    Resp3Frame::SimpleError { data, .. } => Err(pretty_error(&data)),
    Resp3Frame::BlobError { data, .. } => Err(pretty_error(str::from_utf8(&data)?)),
    _ => Err(Error::new(ErrorKind::Protocol, "Expected string.")),
  }
}

/// Parse the frame from a shard pubsub channel.
// TODO clean this up with the v5 redis_protocol interface
pub fn parse_shard_pubsub_frame(server: &Server, frame: &Resp3Frame) -> Option<Message> {
  let value = match frame {
    Resp3Frame::Array { ref data, .. } | Resp3Frame::Push { ref data, .. } => {
      if data.len() >= 3 && data.len() <= 5 {
        // check both resp2 and resp3 formats
        let has_either_prefix = (data[0].as_str().map(|s| s == PUBSUB_PUSH_PREFIX).unwrap_or(false)
          && data[1].as_str().map(|s| s == "smessage").unwrap_or(false))
          || (data[0].as_str().map(|s| s == "smessage").unwrap_or(false));

        if has_either_prefix {
          let channel = frame_to_str(data[data.len() - 2].clone())?;
          let message = match frame_to_results(data[data.len() - 1].clone()) {
            Ok(message) => message,
            Err(_) => return None,
          };

          Some((channel, message))
        } else {
          None
        }
      } else {
        None
      }
    },
    _ => None,
  };

  value.map(|(channel, value)| Message {
    channel,
    value,
    kind: MessageKind::SMessage,
    server: server.clone(),
  })
}

/// Parse the kind of pubsub message (pattern, sharded, or regular).
pub fn parse_message_kind(frame: &Resp3Frame) -> Result<MessageKind, Error> {
  let frames = match frame {
    Resp3Frame::Array { ref data, .. } => data,
    Resp3Frame::Push { ref data, .. } => data,
    _ => return Err(Error::new(ErrorKind::Protocol, "Invalid pubsub frame type.")),
  };

  let parsed = if frames.len() == 3 {
    // resp2 format, normal message
    frames[0].as_str().and_then(MessageKind::from_str)
  } else if frames.len() == 4 {
    // resp3 normal message or resp2 pattern/shard message
    frames[1]
      .as_str()
      .and_then(MessageKind::from_str)
      .or(frames[0].as_str().and_then(MessageKind::from_str))
  } else if frames.len() == 5 {
    // resp3 pattern or shard message
    frames[1]
      .as_str()
      .and_then(MessageKind::from_str)
      .or(frames[2].as_str().and_then(MessageKind::from_str))
  } else {
    None
  };

  parsed.ok_or(Error::new(ErrorKind::Protocol, "Invalid pubsub message kind."))
}

/// Parse the channel and value fields from a pubsub frame.
pub fn parse_message_fields(frame: Resp3Frame) -> Result<(Str, Value), Error> {
  let mut frames = match frame {
    Resp3Frame::Array { data, .. } => data,
    Resp3Frame::Push { data, .. } => data,
    _ => return Err(Error::new(ErrorKind::Protocol, "Invalid pubsub frame type.")),
  };

  let value = frames
    .pop()
    .ok_or(Error::new(ErrorKind::Protocol, "Invalid pubsub message."))?;
  let channel = frames
    .pop()
    .ok_or(Error::new(ErrorKind::Protocol, "Invalid pubsub channel."))?;
  let channel = frame_to_str(channel).ok_or(Error::new(ErrorKind::Protocol, "Failed to parse channel."))?;
  let value = frame_to_results(value)?;

  Ok((channel, value))
}

/// Parse the frame as a pubsub message.
pub fn frame_to_pubsub(server: &Server, frame: Resp3Frame) -> Result<Message, Error> {
  if let Some(message) = parse_shard_pubsub_frame(server, &frame) {
    return Ok(message);
  }

  let kind = parse_message_kind(&frame)?;
  let (channel, value) = parse_message_fields(frame)?;

  Ok(Message {
    kind,
    channel,
    value,
    server: server.clone(),
  })
}

pub fn check_resp2_auth_error(codec: &Codec, frame: Resp2Frame) -> Resp2Frame {
  let is_auth_error = match frame {
    Resp2Frame::Error(ref data) => *data == LEGACY_AUTH_ERROR_BODY || data.starts_with(ACL_AUTH_ERROR_PREFIX),
    _ => false,
  };

  if is_auth_error {
    warn!(
      "{}: [{}] Dropping unused auth warning: {}",
      codec.name,
      codec.server,
      frame.as_str().unwrap_or("")
    );
    Resp2Frame::SimpleString(utils::static_bytes(OK.as_bytes()))
  } else {
    frame
  }
}

pub fn check_resp3_auth_error(codec: &Codec, frame: Resp3Frame) -> Resp3Frame {
  let is_auth_error = match frame {
    Resp3Frame::SimpleError { ref data, .. } => {
      *data == LEGACY_AUTH_ERROR_BODY || data.starts_with(ACL_AUTH_ERROR_PREFIX)
    },
    _ => false,
  };

  if is_auth_error {
    warn!(
      "{}: [{}] Dropping unused auth warning: {}",
      codec.name,
      codec.server,
      frame.as_str().unwrap_or("")
    );
    Resp3Frame::SimpleString {
      data:       utils::static_bytes(OK.as_bytes()),
      attributes: None,
    }
  } else {
    frame
  }
}

/// Try to parse the data as a string, and failing that return a byte slice.
pub fn string_or_bytes(data: Bytes) -> Value {
  if let Ok(s) = Str::from_inner(data.clone()) {
    Value::String(s)
  } else {
    Value::Bytes(data)
  }
}

pub fn frame_to_bytes(frame: Resp3Frame) -> Option<Bytes> {
  match frame {
    Resp3Frame::BigNumber { data, .. } => Some(data),
    Resp3Frame::VerbatimString { data, .. } => Some(data),
    Resp3Frame::BlobString { data, .. } => Some(data),
    Resp3Frame::SimpleString { data, .. } => Some(data),
    Resp3Frame::BlobError { data, .. } => Some(data),
    Resp3Frame::SimpleError { data, .. } => Some(data.into_inner()),
    _ => None,
  }
}

pub fn frame_to_str(frame: Resp3Frame) -> Option<Str> {
  match frame {
    Resp3Frame::BigNumber { data, .. } => Str::from_inner(data).ok(),
    Resp3Frame::VerbatimString { data, .. } => Str::from_inner(data).ok(),
    Resp3Frame::BlobString { data, .. } => Str::from_inner(data).ok(),
    Resp3Frame::SimpleString { data, .. } => Str::from_inner(data).ok(),
    Resp3Frame::BlobError { data, .. } => Str::from_inner(data).ok(),
    Resp3Frame::SimpleError { data, .. } => Some(data),
    _ => None,
  }
}

#[cfg(feature = "i-hashes")]
fn parse_nested_map(data: FrameMap<Resp3Frame, Resp3Frame>) -> Result<Map, Error> {
  let mut out = HashMap::with_capacity(data.len());

  for (key, value) in data.into_iter() {
    let key: Key = frame_to_results(key)?.try_into()?;
    let value = frame_to_results(value)?;

    out.insert(key, value);
  }

  Ok(Map { inner: out })
}

/// Convert `nil` responses to a generic `Timeout` error.
#[cfg(any(feature = "i-lists", feature = "i-sorted-sets"))]
pub fn check_null_timeout(frame: &Resp3Frame) -> Result<(), Error> {
  if frame.kind() == FrameKind::Null {
    Err(Error::new(ErrorKind::Timeout, "Request timed out."))
  } else {
    Ok(())
  }
}

/// Parse the protocol frame into a redis value, with support for arbitrarily nested arrays.
pub fn frame_to_results(frame: Resp3Frame) -> Result<Value, Error> {
  let value = match frame {
    Resp3Frame::Null => Value::Null,
    Resp3Frame::SimpleString { data, .. } => {
      let value = string_or_bytes(data);

      if value.as_str().map(|s| s == QUEUED).unwrap_or(false) {
        Value::Queued
      } else {
        value
      }
    },
    Resp3Frame::SimpleError { data, .. } => return Err(pretty_error(&data)),
    Resp3Frame::BlobString { data, .. } => string_or_bytes(data),
    Resp3Frame::BlobError { data, .. } => {
      let parsed = String::from_utf8_lossy(&data);
      return Err(pretty_error(parsed.as_ref()));
    },
    Resp3Frame::VerbatimString { data, .. } => string_or_bytes(data),
    Resp3Frame::Number { data, .. } => data.into(),
    Resp3Frame::Double { data, .. } => data.into(),
    Resp3Frame::BigNumber { data, .. } => string_or_bytes(data),
    Resp3Frame::Boolean { data, .. } => data.into(),
    Resp3Frame::Array { data, .. } | Resp3Frame::Push { data, .. } => Value::Array(
      data
        .into_iter()
        .map(frame_to_results)
        .collect::<Result<Vec<Value>, _>>()?,
    ),
    Resp3Frame::Set { data, .. } => Value::Array(
      data
        .into_iter()
        .map(frame_to_results)
        .collect::<Result<Vec<Value>, _>>()?,
    ),
    Resp3Frame::Map { data, .. } => {
      let mut out = HashMap::with_capacity(data.len());
      for (key, value) in data.into_iter() {
        let key: Key = frame_to_results(key)?.try_into()?;
        let value = frame_to_results(value)?;

        out.insert(key, value);
      }

      Value::Map(Map { inner: out })
    },
    _ => return Err(Error::new(ErrorKind::Protocol, "Invalid response frame type.")),
  };

  Ok(value)
}

/// Flatten a single nested layer of arrays or sets into an array.
#[cfg(feature = "i-hashes")]
pub fn flatten_frame(frame: Resp3Frame) -> Resp3Frame {
  match frame {
    Resp3Frame::Array { data, .. } => {
      let count = data.iter().fold(0, |c, f| {
        c + match f {
          Resp3Frame::Push { ref data, .. } => data.len(),
          Resp3Frame::Array { ref data, .. } => data.len(),
          Resp3Frame::Set { ref data, .. } => data.len(),
          _ => 1,
        }
      });

      let mut out = Vec::with_capacity(count);
      for frame in data.into_iter() {
        match frame {
          Resp3Frame::Push { data, .. } => out.extend(data),
          Resp3Frame::Array { data, .. } => out.extend(data),
          Resp3Frame::Set { data, .. } => out.extend(data),
          _ => out.push(frame),
        };
      }

      Resp3Frame::Array {
        data:       out,
        attributes: None,
      }
    },
    Resp3Frame::Set { data, .. } => {
      let count = data.iter().fold(0, |c, f| {
        c + match f {
          Resp3Frame::Array { ref data, .. } => data.len(),
          Resp3Frame::Set { ref data, .. } => data.len(),
          _ => 1,
        }
      });

      let mut out = Vec::with_capacity(count);
      for frame in data.into_iter() {
        match frame {
          Resp3Frame::Array { data, .. } => out.extend(data),
          Resp3Frame::Set { data, .. } => out.extend(data),
          _ => out.push(frame),
        };
      }

      Resp3Frame::Array {
        data:       out,
        attributes: None,
      }
    },
    _ => frame,
  }
}

#[cfg(feature = "i-hashes")]
/// Convert a frame to a nested `Map`.
pub fn frame_to_map(frame: Resp3Frame) -> Result<Map, Error> {
  match frame {
    Resp3Frame::Array { mut data, .. } => {
      if data.is_empty() {
        return Ok(Map::new());
      }
      if data.len() % 2 != 0 {
        return Err(Error::new(ErrorKind::Protocol, "Expected an even number of frames."));
      }

      let mut inner = HashMap::with_capacity(data.len() / 2);
      while data.len() >= 2 {
        let value = frame_to_results(data.pop().unwrap())?;
        let key = frame_to_results(data.pop().unwrap())?.try_into()?;

        inner.insert(key, value);
      }

      Ok(Map { inner })
    },
    Resp3Frame::Map { data, .. } => parse_nested_map(data),
    Resp3Frame::SimpleError { data, .. } => Err(pretty_error(&data)),
    Resp3Frame::BlobError { data, .. } => {
      let parsed = String::from_utf8_lossy(&data);
      Err(pretty_error(&parsed))
    },
    _ => Err(Error::new(ErrorKind::Protocol, "Expected array or map frames.")),
  }
}

/// Convert a frame to a `RedisError`.
pub fn frame_to_error(frame: &Resp3Frame) -> Option<Error> {
  match frame {
    Resp3Frame::SimpleError { ref data, .. } => Some(pretty_error(data)),
    Resp3Frame::BlobError { ref data, .. } => {
      let parsed = String::from_utf8_lossy(data);
      Some(pretty_error(parsed.as_ref()))
    },
    _ => None,
  }
}

pub fn value_to_outgoing_resp2_frame(value: &Value) -> Result<Resp2Frame, Error> {
  let frame = match value {
    Value::Double(ref f) => Resp2Frame::BulkString(f.to_string().into()),
    Value::Boolean(ref b) => Resp2Frame::BulkString(b.to_string().into()),
    // the `int_as_bulkstring` flag in redis-protocol converts this to a bulk string
    Value::Integer(ref i) => Resp2Frame::Integer(*i),
    Value::String(ref s) => Resp2Frame::BulkString(s.inner().clone()),
    Value::Bytes(ref b) => Resp2Frame::BulkString(b.clone()),
    Value::Queued => Resp2Frame::BulkString(Bytes::from_static(QUEUED.as_bytes())),
    Value::Null => Resp2Frame::Null,
    _ => {
      return Err(Error::new(
        ErrorKind::InvalidArgument,
        format!("Invalid argument type: {}", value.kind()),
      ))
    },
  };

  Ok(frame)
}

pub fn value_to_outgoing_resp3_frame(value: &Value) -> Result<Resp3Frame, Error> {
  let frame = match value {
    Value::Double(ref f) => Resp3Frame::BlobString {
      data:       f.to_string().into(),
      attributes: None,
    },
    Value::Boolean(ref b) => Resp3Frame::BlobString {
      data:       b.to_string().into(),
      attributes: None,
    },
    // the `int_as_blobstring` flag in redis-protocol converts this to a blob string
    Value::Integer(ref i) => Resp3Frame::Number {
      data:       *i,
      attributes: None,
    },
    Value::String(ref s) => Resp3Frame::BlobString {
      data:       s.inner().clone(),
      attributes: None,
    },
    Value::Bytes(ref b) => Resp3Frame::BlobString {
      data:       b.clone(),
      attributes: None,
    },
    Value::Queued => Resp3Frame::BlobString {
      data:       Bytes::from_static(QUEUED.as_bytes()),
      attributes: None,
    },
    Value::Null => Resp3Frame::Null,
    _ => {
      return Err(Error::new(
        ErrorKind::InvalidArgument,
        format!("Invalid argument type: {}", value.kind()),
      ))
    },
  };

  Ok(frame)
}

#[cfg(feature = "mocks")]
pub fn mocked_value_to_frame(value: Value) -> Resp3Frame {
  match value {
    Value::Array(values) => Resp3Frame::Array {
      data:       values.into_iter().map(mocked_value_to_frame).collect(),
      attributes: None,
    },
    Value::Map(values) => Resp3Frame::Map {
      data:       values
        .inner()
        .into_iter()
        .map(|(key, value)| (mocked_value_to_frame(key.into()), mocked_value_to_frame(value)))
        .collect(),
      attributes: None,
    },
    Value::Null => Resp3Frame::Null,
    Value::Queued => Resp3Frame::SimpleString {
      data:       Bytes::from_static(QUEUED.as_bytes()),
      attributes: None,
    },
    Value::Bytes(value) => Resp3Frame::BlobString {
      data:       value,
      attributes: None,
    },
    Value::Boolean(value) => Resp3Frame::Boolean {
      data:       value,
      attributes: None,
    },
    Value::Integer(value) => Resp3Frame::Number {
      data:       value,
      attributes: None,
    },
    Value::Double(value) => Resp3Frame::Double {
      data:       value,
      attributes: None,
    },
    Value::String(value) => Resp3Frame::BlobString {
      data:       value.into_inner(),
      attributes: None,
    },
  }
}

pub fn expect_ok(value: &Value) -> Result<(), Error> {
  match *value {
    Value::String(ref resp) => {
      if resp.deref() == OK || resp.deref() == QUEUED {
        Ok(())
      } else {
        Err(Error::new(ErrorKind::Unknown, format!("Expected OK, found {}", resp)))
      }
    },
    _ => Err(Error::new(
      ErrorKind::Unknown,
      format!("Expected OK, found {:?}.", value),
    )),
  }
}

/// Parse the replicas from the ROLE response returned from a master/primary node.
#[cfg(feature = "replicas")]
pub fn parse_master_role_replicas(data: Value) -> Result<Vec<Server>, Error> {
  let mut role: Vec<Value> = data.convert()?;

  if role.len() == 3 {
    if role[0].as_str().map(|s| s == "master").unwrap_or(false) {
      let replicas: Vec<Value> = role[2].take().convert()?;

      Ok(
        replicas
          .into_iter()
          .filter_map(|value| {
            value
              .convert::<(String, u16, String)>()
              .ok()
              .map(|(host, port, _)| Server::new(host, port))
          })
          .collect(),
      )
    } else {
      Ok(Vec::new())
    }
  } else {
    // we're talking to a replica or sentinel node
    Ok(Vec::new())
  }
}

#[cfg(feature = "i-geo")]
pub fn assert_array_len<T>(data: &[T], len: usize) -> Result<(), Error> {
  if data.len() == len {
    Ok(())
  } else {
    Err(Error::new(ErrorKind::Parse, format!("Expected {} values.", len)))
  }
}

/// Flatten a nested array of values into one array.
pub fn flatten_value(value: Value) -> Value {
  if let Value::Array(values) = value {
    let mut out = Vec::with_capacity(values.len());
    for value in values.into_iter() {
      let flattened = flatten_value(value);
      if let Value::Array(flattened) = flattened {
        out.extend(flattened);
      } else {
        out.push(flattened);
      }
    }

    Value::Array(out)
  } else {
    value
  }
}

/// Convert a redis value to an array of (value, score) tuples.
pub fn value_to_zset_result(value: Value) -> Result<Vec<(Value, f64)>, Error> {
  let value = flatten_value(value);

  if let Value::Array(mut values) = value {
    if values.is_empty() {
      return Ok(Vec::new());
    }
    if values.len() % 2 != 0 {
      return Err(Error::new(
        ErrorKind::Unknown,
        "Expected an even number of redis values.",
      ));
    }

    let mut out = Vec::with_capacity(values.len() / 2);
    while values.len() >= 2 {
      let score = match values.pop().unwrap().as_f64() {
        Some(f) => f,
        None => {
          return Err(Error::new(
            ErrorKind::Protocol,
            "Could not convert value to floating point number.",
          ))
        },
      };
      let value = values.pop().unwrap();

      out.push((value, score));
    }

    Ok(out)
  } else {
    Err(Error::new(ErrorKind::Unknown, "Expected array of redis values."))
  }
}

#[cfg(any(feature = "blocking-encoding", feature = "partial-tracing", feature = "full-tracing"))]
fn i64_size(i: i64) -> usize {
  if i < 0 {
    1 + redis_protocol::digits_in_usize(-i as usize)
  } else {
    redis_protocol::digits_in_usize(i as usize)
  }
}

#[cfg(any(feature = "blocking-encoding", feature = "partial-tracing", feature = "full-tracing"))]
pub fn arg_size(value: &Value) -> usize {
  match value {
    // use the RESP2 size
    Value::Boolean(_) => 5,
    // TODO try digits_in_number(f.trunc()) + 1 + digits_in_number(f.fract())
    // but don't forget the negative sign byte
    Value::Double(_) => 10,
    Value::Null => 3,
    Value::Integer(ref i) => i64_size(*i),
    Value::String(ref s) => s.inner().len(),
    Value::Bytes(ref b) => b.len(),
    Value::Array(ref arr) => args_size(arr),
    Value::Map(ref map) => map
      .inner
      .iter()
      .fold(0, |c, (k, v)| c + k.as_bytes().len() + arg_size(v)),
    Value::Queued => 0,
  }
}

#[cfg(any(feature = "blocking-encoding", feature = "partial-tracing", feature = "full-tracing"))]
pub fn args_size(args: &[Value]) -> usize {
  args.iter().fold(0, |c, arg| c + arg_size(arg))
}

fn serialize_hello(command: &Command, version: &RespVersion) -> Result<ProtocolFrame, Error> {
  let args = command.args();

  let (auth, setname) = if args.len() == 3 {
    // has auth and setname
    let username = match args[0].as_bytes_str() {
      Some(username) => username,
      None => {
        return Err(Error::new(
          ErrorKind::InvalidArgument,
          "Invalid username. Expected string.",
        ));
      },
    };
    let password = match args[1].as_bytes_str() {
      Some(password) => password,
      None => {
        return Err(Error::new(
          ErrorKind::InvalidArgument,
          "Invalid password. Expected string.",
        ));
      },
    };
    let name = match args[2].as_bytes_str() {
      Some(val) => val,
      None => {
        return Err(Error::new(
          ErrorKind::InvalidArgument,
          "Invalid setname value. Expected string.",
        ));
      },
    };

    (Some((username, password)), Some(name))
  } else if args.len() == 2 {
    // has auth but no setname
    let username = match args[0].as_bytes_str() {
      Some(username) => username,
      None => {
        return Err(Error::new(
          ErrorKind::InvalidArgument,
          "Invalid username. Expected string.",
        ));
      },
    };
    let password = match args[1].as_bytes_str() {
      Some(password) => password,
      None => {
        return Err(Error::new(
          ErrorKind::InvalidArgument,
          "Invalid password. Expected string.",
        ));
      },
    };

    (Some((username, password)), None)
  } else if args.len() == 1 {
    // has setname but no auth
    let name = match args[0].as_bytes_str() {
      Some(val) => val,
      None => {
        return Err(Error::new(
          ErrorKind::InvalidArgument,
          "Invalid setname value. Expected string.",
        ));
      },
    };

    (None, Some(name))
  } else {
    (None, None)
  };

  Ok(ProtocolFrame::Resp3(Resp3Frame::Hello {
    version: version.clone(),
    auth,
    setname,
  }))
}

// TODO find a way to optimize these functions to use borrowed frame types
pub fn command_to_resp3_frame(command: &Command) -> Result<ProtocolFrame, Error> {
  let args = command.args();

  match command.kind {
    CommandKind::_Custom(ref kind) => {
      let parts: Vec<&str> = kind.cmd.trim().split(' ').collect();
      let mut bulk_strings = Vec::with_capacity(parts.len() + args.len());
      for part in parts.into_iter() {
        bulk_strings.push(Resp3Frame::BlobString {
          data:       part.as_bytes().to_vec().into(),
          attributes: None,
        });
      }
      for value in args.iter() {
        bulk_strings.push(value_to_outgoing_resp3_frame(value)?);
      }

      Ok(ProtocolFrame::Resp3(Resp3Frame::Array {
        data:       bulk_strings,
        attributes: None,
      }))
    },
    CommandKind::_HelloAllCluster(ref version) | CommandKind::_Hello(ref version) => {
      serialize_hello(command, version)
    },
    _ => {
      let mut bulk_strings = Vec::with_capacity(args.len() + 2);

      bulk_strings.push(Resp3Frame::BlobString {
        data:       command.kind.cmd_str().into_inner(),
        attributes: None,
      });

      if let Some(subcommand) = command.kind.subcommand_str() {
        bulk_strings.push(Resp3Frame::BlobString {
          data:       subcommand.into_inner(),
          attributes: None,
        });
      }
      for value in args.iter() {
        bulk_strings.push(value_to_outgoing_resp3_frame(value)?);
      }

      Ok(ProtocolFrame::Resp3(Resp3Frame::Array {
        data:       bulk_strings,
        attributes: None,
      }))
    },
  }
}

pub fn command_to_resp2_frame(command: &Command) -> Result<ProtocolFrame, Error> {
  let args = command.args();

  match command.kind {
    CommandKind::_Custom(ref kind) => {
      let parts: Vec<&str> = kind.cmd.trim().split(' ').collect();
      let mut bulk_strings = Vec::with_capacity(parts.len() + args.len());

      for part in parts.into_iter() {
        bulk_strings.push(Resp2Frame::BulkString(part.as_bytes().to_vec().into()));
      }
      for value in args.iter() {
        bulk_strings.push(value_to_outgoing_resp2_frame(value)?);
      }

      Ok(Resp2Frame::Array(bulk_strings).into())
    },
    _ => {
      let mut bulk_strings = Vec::with_capacity(args.len() + 2);

      bulk_strings.push(Resp2Frame::BulkString(command.kind.cmd_str().into_inner()));
      if let Some(subcommand) = command.kind.subcommand_str() {
        bulk_strings.push(Resp2Frame::BulkString(subcommand.into_inner()));
      }
      for value in args.iter() {
        bulk_strings.push(value_to_outgoing_resp2_frame(value)?);
      }

      Ok(Resp2Frame::Array(bulk_strings).into())
    },
  }
}

/// Serialize the command as a protocol frame.
pub fn command_to_frame(command: &Command, is_resp3: bool) -> Result<ProtocolFrame, Error> {
  if is_resp3 || command.kind.is_hello() {
    command_to_resp3_frame(command)
  } else {
    command_to_resp2_frame(command)
  }
}

pub fn encode_frame(inner: &RefCount<ClientInner>, command: &Command) -> Result<ProtocolFrame, Error> {
  #[cfg(all(feature = "blocking-encoding", not(feature = "glommio")))]
  return command.to_frame_blocking(
    inner.is_resp3(),
    inner.with_perf_config(|c| c.blocking_encode_threshold),
  );

  #[cfg(any(
    not(feature = "blocking-encoding"),
    all(feature = "blocking-encoding", feature = "glommio")
  ))]
  command.to_frame(inner.is_resp3())
}

#[cfg(test)]
mod tests {
  #![allow(dead_code)]
  #![allow(unused_imports)]
  use super::*;
  #[cfg(feature = "i-cluster")]
  use crate::types::cluster::{ClusterInfo, ClusterState};
  use std::{collections::HashMap, time::Duration};

  fn str_to_f(s: &str) -> Resp3Frame {
    Resp3Frame::SimpleString {
      data:       s.to_owned().into(),
      attributes: None,
    }
  }

  fn str_to_bs(s: &str) -> Resp3Frame {
    Resp3Frame::BlobString {
      data:       s.to_owned().into(),
      attributes: None,
    }
  }

  fn int_to_f(i: i64) -> Resp3Frame {
    Resp3Frame::Number {
      data:       i,
      attributes: None,
    }
  }

  #[test]
  #[cfg(feature = "i-memory")]
  fn should_parse_memory_stats() {
    // better from()/into() interfaces for frames coming in the next redis-protocol version...
    let input = frame_to_results(Resp3Frame::Array {
      data:       vec![
        str_to_f("peak.allocated"),
        int_to_f(934192),
        str_to_f("total.allocated"),
        int_to_f(872040),
        str_to_f("startup.allocated"),
        int_to_f(809912),
        str_to_f("replication.backlog"),
        int_to_f(0),
        str_to_f("clients.slaves"),
        int_to_f(0),
        str_to_f("clients.normal"),
        int_to_f(20496),
        str_to_f("aof.buffer"),
        int_to_f(0),
        str_to_f("lua.caches"),
        int_to_f(0),
        str_to_f("db.0"),
        Resp3Frame::Array {
          data:       vec![
            str_to_f("overhead.hashtable.main"),
            int_to_f(72),
            str_to_f("overhead.hashtable.expires"),
            int_to_f(0),
          ],
          attributes: None,
        },
        str_to_f("overhead.total"),
        int_to_f(830480),
        str_to_f("keys.count"),
        int_to_f(1),
        str_to_f("keys.bytes-per-key"),
        int_to_f(62128),
        str_to_f("dataset.bytes"),
        int_to_f(41560),
        str_to_f("dataset.percentage"),
        str_to_f("66.894157409667969"),
        str_to_f("peak.percentage"),
        str_to_f("93.346977233886719"),
        str_to_f("allocator.allocated"),
        int_to_f(1022640),
        str_to_f("allocator.active"),
        int_to_f(1241088),
        str_to_f("allocator.resident"),
        int_to_f(5332992),
        str_to_f("allocator-fragmentation.ratio"),
        str_to_f("1.2136118412017822"),
        str_to_f("allocator-fragmentation.bytes"),
        int_to_f(218448),
        str_to_f("allocator-rss.ratio"),
        str_to_f("4.2970294952392578"),
        str_to_f("allocator-rss.bytes"),
        int_to_f(4091904),
        str_to_f("rss-overhead.ratio"),
        str_to_f("2.0268816947937012"),
        str_to_f("rss-overhead.bytes"),
        int_to_f(5476352),
        str_to_f("fragmentation"),
        str_to_f("13.007383346557617"),
        str_to_f("fragmentation.bytes"),
        int_to_f(9978328),
      ],
      attributes: None,
    })
    .unwrap();
    let memory_stats: MemoryStats = input.convert().unwrap();

    let expected_db_0 = DatabaseMemoryStats {
      overhead_hashtable_expires:      0,
      overhead_hashtable_main:         72,
      overhead_hashtable_slot_to_keys: 0,
    };
    let mut expected_db = HashMap::new();
    expected_db.insert(0, expected_db_0);
    let expected = MemoryStats {
      peak_allocated:                934192,
      total_allocated:               872040,
      startup_allocated:             809912,
      replication_backlog:           0,
      clients_slaves:                0,
      clients_normal:                20496,
      aof_buffer:                    0,
      lua_caches:                    0,
      db:                            expected_db,
      overhead_total:                830480,
      keys_count:                    1,
      keys_bytes_per_key:            62128,
      dataset_bytes:                 41560,
      dataset_percentage:            66.894_157_409_667_97,
      peak_percentage:               93.346_977_233_886_72,
      allocator_allocated:           1022640,
      allocator_active:              1241088,
      allocator_resident:            5332992,
      allocator_fragmentation_ratio: 1.2136118412017822,
      allocator_fragmentation_bytes: 218448,
      allocator_rss_ratio:           4.297_029_495_239_258,
      allocator_rss_bytes:           4091904,
      rss_overhead_ratio:            2.026_881_694_793_701,
      rss_overhead_bytes:            5476352,
      fragmentation:                 13.007383346557617,
      fragmentation_bytes:           9978328,
    };

    assert_eq!(memory_stats, expected);
  }

  #[test]
  #[cfg(feature = "i-slowlog")]
  fn should_parse_slowlog_entries_redis_3() {
    // redis 127.0.0.1:6379> slowlog get 2
    // 1) 1) (integer) 14
    // 2) (integer) 1309448221
    // 3) (integer) 15
    // 4) 1) "ping"
    // 2) 1) (integer) 13
    // 2) (integer) 1309448128
    // 3) (integer) 30
    // 4) 1) "slowlog"
    // 2) "get"
    // 3) "100"

    let input = frame_to_results(Resp3Frame::Array {
      data:       vec![
        Resp3Frame::Array {
          data:       vec![int_to_f(14), int_to_f(1309448221), int_to_f(15), Resp3Frame::Array {
            data:       vec![str_to_bs("ping")],
            attributes: None,
          }],
          attributes: None,
        },
        Resp3Frame::Array {
          data:       vec![int_to_f(13), int_to_f(1309448128), int_to_f(30), Resp3Frame::Array {
            data:       vec![str_to_bs("slowlog"), str_to_bs("get"), str_to_bs("100")],
            attributes: None,
          }],
          attributes: None,
        },
      ],
      attributes: None,
    })
    .unwrap();
    let actual: Vec<SlowlogEntry> = input.convert().unwrap();

    let expected = vec![
      SlowlogEntry {
        id:        14,
        timestamp: 1309448221,
        duration:  Duration::from_micros(15),
        args:      vec!["ping".into()],
        ip:        None,
        name:      None,
      },
      SlowlogEntry {
        id:        13,
        timestamp: 1309448128,
        duration:  Duration::from_micros(30),
        args:      vec!["slowlog".into(), "get".into(), "100".into()],
        ip:        None,
        name:      None,
      },
    ];

    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "i-slowlog")]
  fn should_parse_slowlog_entries_redis_4() {
    // redis 127.0.0.1:6379> slowlog get 2
    // 1) 1) (integer) 14
    // 2) (integer) 1309448221
    // 3) (integer) 15
    // 4) 1) "ping"
    // 5) "127.0.0.1:58217"
    // 6) "worker-123"
    // 2) 1) (integer) 13
    // 2) (integer) 1309448128
    // 3) (integer) 30
    // 4) 1) "slowlog"
    // 2) "get"
    // 3) "100"
    // 5) "127.0.0.1:58217"
    // 6) "worker-123"

    let input = frame_to_results(Resp3Frame::Array {
      data:       vec![
        Resp3Frame::Array {
          data:       vec![
            int_to_f(14),
            int_to_f(1309448221),
            int_to_f(15),
            Resp3Frame::Array {
              data:       vec![str_to_bs("ping")],
              attributes: None,
            },
            str_to_bs("127.0.0.1:58217"),
            str_to_bs("worker-123"),
          ],
          attributes: None,
        },
        Resp3Frame::Array {
          data:       vec![
            int_to_f(13),
            int_to_f(1309448128),
            int_to_f(30),
            Resp3Frame::Array {
              data:       vec![str_to_bs("slowlog"), str_to_bs("get"), str_to_bs("100")],
              attributes: None,
            },
            str_to_bs("127.0.0.1:58217"),
            str_to_bs("worker-123"),
          ],
          attributes: None,
        },
      ],
      attributes: None,
    })
    .unwrap();
    let actual: Vec<SlowlogEntry> = input.convert().unwrap();

    let expected = vec![
      SlowlogEntry {
        id:        14,
        timestamp: 1309448221,
        duration:  Duration::from_micros(15),
        args:      vec!["ping".into()],
        ip:        Some("127.0.0.1:58217".into()),
        name:      Some("worker-123".into()),
      },
      SlowlogEntry {
        id:        13,
        timestamp: 1309448128,
        duration:  Duration::from_micros(30),
        args:      vec!["slowlog".into(), "get".into(), "100".into()],
        ip:        Some("127.0.0.1:58217".into()),
        name:      Some("worker-123".into()),
      },
    ];

    assert_eq!(actual, expected);
  }

  #[test]
  #[cfg(feature = "i-cluster")]
  fn should_parse_cluster_info() {
    let input: Value = "cluster_state:fail
cluster_slots_assigned:16384
cluster_slots_ok:16384
cluster_slots_pfail:3
cluster_slots_fail:2
cluster_known_nodes:6
cluster_size:3
cluster_current_epoch:6
cluster_my_epoch:2
cluster_stats_messages_sent:1483972
cluster_stats_messages_received:1483968"
      .into();

    let expected = ClusterInfo {
      cluster_state:                   ClusterState::Fail,
      cluster_slots_assigned:          16384,
      cluster_slots_ok:                16384,
      cluster_slots_fail:              2,
      cluster_slots_pfail:             3,
      cluster_known_nodes:             6,
      cluster_size:                    3,
      cluster_current_epoch:           6,
      cluster_my_epoch:                2,
      cluster_stats_messages_sent:     1483972,
      cluster_stats_messages_received: 1483968,
    };
    let actual: ClusterInfo = input.convert().unwrap();

    assert_eq!(actual, expected);
  }
}
