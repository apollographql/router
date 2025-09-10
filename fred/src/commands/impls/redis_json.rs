use crate::{
  error::{Error, ErrorKind},
  interfaces::{ClientLike, FredResult},
  protocol::{command::CommandKind, utils as protocol_utils},
  types::{Key, MultipleKeys, MultipleStrings, SetOptions, Value},
  utils,
};
use bytes_utils::Str;

const INDENT: &str = "INDENT";
const NEWLINE: &str = "NEWLINE";
const SPACE: &str = "SPACE";

fn key_path_args(key: Key, path: Option<Str>, extra: usize) -> Vec<Value> {
  let mut out = Vec::with_capacity(2 + extra);
  out.push(key.into());
  if let Some(path) = path {
    out.push(path.into());
  }
  out
}

/// Convert the provided json value to a redis value by serializing into a json string.
fn value_to_bulk_str(value: &serde_json::Value) -> Result<Value, Error> {
  Ok(match value {
    serde_json::Value::String(ref s) => Value::String(Str::from(s)),
    _ => Value::String(Str::from(serde_json::to_string(value)?)),
  })
}

/// Convert the provided json value to a `Value` directly without serializing into a string. This only works with
/// scalar values.
fn json_to_value(value: serde_json::Value) -> Result<Value, Error> {
  let out = match value {
    serde_json::Value::String(s) => Some(Value::String(Str::from(s))),
    serde_json::Value::Null => Some(Value::Null),
    serde_json::Value::Number(n) => {
      if n.is_f64() {
        n.as_f64().map(Value::Double)
      } else {
        n.as_i64().map(Value::Integer)
      }
    },
    serde_json::Value::Bool(b) => Some(Value::Boolean(b)),
    _ => None,
  };

  out.ok_or(Error::new(ErrorKind::InvalidArgument, "Expected string or number."))
}

fn values_to_bulk(values: &[serde_json::Value]) -> Result<Vec<Value>, Error> {
  values.iter().map(value_to_bulk_str).collect()
}

pub async fn json_arrappend<C: ClientLike>(
  client: &C,
  key: Key,
  path: Str,
  values: Vec<serde_json::Value>,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = key_path_args(key, Some(path), values.len());
    args.extend(values_to_bulk(&values)?);

    Ok((CommandKind::JsonArrAppend, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_arrindex<C: ClientLike>(
  client: &C,
  key: Key,
  path: Str,
  value: serde_json::Value,
  start: Option<i64>,
  stop: Option<i64>,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = Vec::with_capacity(5);
    args.extend([key.into(), path.into(), value_to_bulk_str(&value)?]);
    if let Some(start) = start {
      args.push(start.into());
    }
    if let Some(stop) = stop {
      args.push(stop.into());
    }

    Ok((CommandKind::JsonArrIndex, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_arrinsert<C: ClientLike>(
  client: &C,
  key: Key,
  path: Str,
  index: i64,
  values: Vec<serde_json::Value>,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = Vec::with_capacity(3 + values.len());
    args.extend([key.into(), path.into(), index.into()]);
    args.extend(values_to_bulk(&values)?);

    Ok((CommandKind::JsonArrInsert, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_arrlen<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || Ok((CommandKind::JsonArrLen, key_path_args(key, path, 0)))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_arrpop<C: ClientLike>(
  client: &C,
  key: Key,
  path: Option<Str>,
  index: Option<i64>,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = key_path_args(key, path, 1);
    if let Some(index) = index {
      args.push(index.into());
    }

    Ok((CommandKind::JsonArrPop, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_arrtrim<C: ClientLike>(
  client: &C,
  key: Key,
  path: Str,
  start: i64,
  stop: i64,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    Ok((CommandKind::JsonArrTrim, vec![
      key.into(),
      path.into(),
      start.into(),
      stop.into(),
    ]))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_clear<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || Ok((CommandKind::JsonClear, key_path_args(key, path, 0)))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_debug_memory<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    Ok((CommandKind::JsonDebugMemory, key_path_args(key, path, 0)))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_del<C: ClientLike>(client: &C, key: Key, path: Str) -> FredResult<Value> {
  let frame =
    utils::request_response(client, || Ok((CommandKind::JsonDel, key_path_args(key, Some(path), 0)))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_get<C: ClientLike>(
  client: &C,
  key: Key,
  indent: Option<Str>,
  newline: Option<Str>,
  space: Option<Str>,
  paths: MultipleStrings,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = Vec::with_capacity(7 + paths.len());
    args.push(key.into());
    if let Some(indent) = indent {
      args.push(static_val!(INDENT));
      args.push(indent.into());
    }
    if let Some(newline) = newline {
      args.push(static_val!(NEWLINE));
      args.push(newline.into());
    }
    if let Some(space) = space {
      args.push(static_val!(SPACE));
      args.push(space.into());
    }
    args.extend(paths.into_values());

    Ok((CommandKind::JsonGet, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_merge<C: ClientLike>(
  client: &C,
  key: Key,
  path: Str,
  value: serde_json::Value,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    Ok((CommandKind::JsonMerge, vec![
      key.into(),
      path.into(),
      value_to_bulk_str(&value)?,
    ]))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_mget<C: ClientLike>(client: &C, keys: MultipleKeys, path: Str) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = Vec::with_capacity(keys.len() + 1);
    args.extend(keys.into_values());
    args.push(path.into());

    Ok((CommandKind::JsonMGet, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_mset<C: ClientLike>(client: &C, values: Vec<(Key, Str, serde_json::Value)>) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = Vec::with_capacity(values.len() * 3);
    for (key, path, value) in values.into_iter() {
      args.extend([key.into(), path.into(), value_to_bulk_str(&value)?]);
    }

    Ok((CommandKind::JsonMSet, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_numincrby<C: ClientLike>(
  client: &C,
  key: Key,
  path: Str,
  value: serde_json::Value,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    Ok((CommandKind::JsonNumIncrBy, vec![
      key.into(),
      path.into(),
      json_to_value(value)?,
    ]))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_objkeys<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || Ok((CommandKind::JsonObjKeys, key_path_args(key, path, 0)))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_objlen<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || Ok((CommandKind::JsonObjLen, key_path_args(key, path, 0)))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_resp<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || Ok((CommandKind::JsonResp, key_path_args(key, path, 0)))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_set<C: ClientLike>(
  client: &C,
  key: Key,
  path: Str,
  value: serde_json::Value,
  options: Option<SetOptions>,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = key_path_args(key, Some(path), 2);
    args.push(value_to_bulk_str(&value)?);
    if let Some(options) = options {
      args.push(options.to_str().into());
    }

    Ok((CommandKind::JsonSet, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_strappend<C: ClientLike>(
  client: &C,
  key: Key,
  path: Option<Str>,
  value: serde_json::Value,
) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    let mut args = key_path_args(key, path, 1);
    args.push(value_to_bulk_str(&value)?);

    Ok((CommandKind::JsonStrAppend, args))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_strlen<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || Ok((CommandKind::JsonStrLen, key_path_args(key, path, 0)))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_toggle<C: ClientLike>(client: &C, key: Key, path: Str) -> FredResult<Value> {
  let frame = utils::request_response(client, || {
    Ok((CommandKind::JsonToggle, key_path_args(key, Some(path), 0)))
  })
  .await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn json_type<C: ClientLike>(client: &C, key: Key, path: Option<Str>) -> FredResult<Value> {
  let frame = utils::request_response(client, || Ok((CommandKind::JsonType, key_path_args(key, path, 0)))).await?;
  protocol_utils::frame_to_results(frame)
}
