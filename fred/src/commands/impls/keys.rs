use super::*;
use crate::{
  error::*,
  protocol::{command::CommandKind, utils as protocol_utils},
  types::*,
  utils,
};
use std::convert::TryInto;

fn check_empty_keys(keys: &MultipleKeys) -> Result<(), Error> {
  if keys.len() == 0 {
    Err(Error::new(ErrorKind::InvalidArgument, "At least one key is required."))
  } else {
    Ok(())
  }
}

value_cmd!(randomkey, Randomkey);

pub async fn get<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::Get, key.into()).await
}

pub async fn set<C: ClientLike>(
  client: &C,
  key: Key,
  value: Value,
  expire: Option<Expiration>,
  options: Option<SetOptions>,
  get: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(6);
    args.push(key.into());
    args.push(value);

    if let Some(expire) = expire {
      let (k, v) = expire.into_args();
      args.push(k.into());
      if let Some(v) = v {
        args.push(v.into());
      }
    }
    if let Some(options) = options {
      args.push(options.to_str().into());
    }
    if get {
      args.push(static_val!(GET));
    }

    Ok((CommandKind::Set, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn setnx<C: ClientLike>(client: &C, key: Key, value: Value) -> Result<Value, Error> {
  args_value_cmd(client, CommandKind::Setnx, vec![key.into(), value]).await
}

pub async fn del<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  check_empty_keys(&keys)?;

  let args: Vec<Value> = keys.inner().drain(..).map(|k| k.into()).collect();
  let frame = utils::request_response(client, move || Ok((CommandKind::Del, args))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn unlink<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  check_empty_keys(&keys)?;

  let args: Vec<Value> = keys.inner().drain(..).map(|k| k.into()).collect();
  let frame = utils::request_response(client, move || Ok((CommandKind::Unlink, args))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn append<C: ClientLike>(client: &C, key: Key, value: Value) -> Result<Value, Error> {
  args_value_cmd(client, CommandKind::Append, vec![key.into(), value]).await
}

pub async fn incr<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Incr, key.into()).await
}

pub async fn decr<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Decr, key.into()).await
}

pub async fn incr_by<C: ClientLike>(client: &C, key: Key, val: i64) -> Result<Value, Error> {
  let frame =
    utils::request_response(client, move || Ok((CommandKind::IncrBy, vec![key.into(), val.into()]))).await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn decr_by<C: ClientLike>(client: &C, key: Key, val: i64) -> Result<Value, Error> {
  let frame =
    utils::request_response(client, move || Ok((CommandKind::DecrBy, vec![key.into(), val.into()]))).await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn incr_by_float<C: ClientLike>(client: &C, key: Key, val: f64) -> Result<Value, Error> {
  let val: Value = val.try_into()?;
  let frame = utils::request_response(client, move || Ok((CommandKind::IncrByFloat, vec![key.into(), val]))).await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ttl<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Ttl, key.into()).await
}

pub async fn pttl<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Pttl, key.into()).await
}

pub async fn persist<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Persist, key.into()).await
}

pub async fn expire<C: ClientLike>(
  client: &C,
  key: Key,
  seconds: i64,
  options: Option<ExpireOptions>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = if let Some(options) = options {
      vec![key.into(), seconds.into(), options.to_str().into()]
    } else {
      vec![key.into(), seconds.into()]
    };

    Ok((CommandKind::Expire, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn expire_at<C: ClientLike>(
  client: &C,
  key: Key,
  timestamp: i64,
  options: Option<ExpireOptions>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = if let Some(options) = options {
      vec![key.into(), timestamp.into(), options.to_str().into()]
    } else {
      vec![key.into(), timestamp.into()]
    };

    Ok((CommandKind::ExpireAt, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn expire_time<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::ExpireTime, key.into()).await
}

pub async fn pexpire_time<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::PexpireTime, key.into()).await
}

pub async fn pexpire<C: ClientLike>(
  client: &C,
  key: Key,
  milliseconds: i64,
  options: Option<ExpireOptions>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = if let Some(options) = options {
      vec![key.into(), milliseconds.into(), options.to_str().into()]
    } else {
      vec![key.into(), milliseconds.into()]
    };

    Ok((CommandKind::Pexpire, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn pexpire_at<C: ClientLike>(
  client: &C,
  key: Key,
  timestamp: i64,
  options: Option<ExpireOptions>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = if let Some(options) = options {
      vec![key.into(), timestamp.into(), options.to_str().into()]
    } else {
      vec![key.into(), timestamp.into()]
    };

    Ok((CommandKind::Pexpireat, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn exists<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  check_empty_keys(&keys)?;

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(keys.len());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }

    Ok((CommandKind::Exists, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn dump<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::Dump, key.into()).await
}

pub async fn restore<C: ClientLike>(
  client: &C,
  key: Key,
  ttl: i64,
  serialized: Value,
  replace: bool,
  absttl: bool,
  idletime: Option<i64>,
  frequency: Option<i64>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(9);
    args.push(key.into());
    args.push(ttl.into());
    args.push(serialized);

    if replace {
      args.push(static_val!(REPLACE));
    }
    if absttl {
      args.push(static_val!(ABSTTL));
    }
    if let Some(idletime) = idletime {
      args.push(static_val!(IDLE_TIME));
      args.push(idletime.into());
    }
    if let Some(frequency) = frequency {
      args.push(static_val!(FREQ));
      args.push(frequency.into());
    }

    Ok((CommandKind::Restore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn getrange<C: ClientLike>(client: &C, key: Key, start: usize, end: usize) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::GetRange, vec![
      key.into(),
      start.try_into()?,
      end.try_into()?,
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn setrange<C: ClientLike>(client: &C, key: Key, offset: u32, value: Value) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Setrange, vec![key.into(), offset.into(), value]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn getset<C: ClientLike>(client: &C, key: Key, value: Value) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::GetSet, vec![key.into(), value]).await
}

pub async fn rename<C: ClientLike>(client: &C, source: Key, destination: Key) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::Rename, vec![source.into(), destination.into()]).await
}

pub async fn renamenx<C: ClientLike>(client: &C, source: Key, destination: Key) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::Renamenx, vec![source.into(), destination.into()]).await
}

pub async fn getdel<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::GetDel, key.into()).await
}

pub async fn strlen<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Strlen, key.into()).await
}

pub async fn mget<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  check_empty_keys(&keys)?;

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(keys.len());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }

    Ok((CommandKind::Mget, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn mset<C: ClientLike>(client: &C, values: Map) -> Result<Value, Error> {
  if values.len() == 0 {
    return Err(Error::new(ErrorKind::InvalidArgument, "Values cannot be empty."));
  }

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(values.len() * 2);

    for (key, value) in values.inner().into_iter() {
      args.push(key.into());
      args.push(value);
    }

    Ok((CommandKind::Mset, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn msetnx<C: ClientLike>(client: &C, values: Map) -> Result<Value, Error> {
  if values.len() == 0 {
    return Err(Error::new(ErrorKind::InvalidArgument, "Values cannot be empty."));
  }

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(values.len() * 2);

    for (key, value) in values.inner().into_iter() {
      args.push(key.into());
      args.push(value);
    }

    Ok((CommandKind::Msetnx, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn copy<C: ClientLike>(
  client: &C,
  source: Key,
  destination: Key,
  db: Option<u8>,
  replace: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(5);
    args.push(source.into());
    args.push(destination.into());

    if let Some(db) = db {
      args.push(static_val!(DB));
      args.push((db as i64).into());
    }
    if replace {
      args.push(static_val!(REPLACE));
    }

    Ok((CommandKind::Copy, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn watch<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<(), Error> {
  let args = keys.inner().into_iter().map(|k| k.into()).collect();
  args_ok_cmd(client, CommandKind::Watch, args).await
}

ok_cmd!(unwatch, Unwatch);

pub async fn r#type<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Type, key.into()).await
}

pub async fn lcs<C: ClientLike>(
  client: &C,
  key1: Key,
  key2: Key,
  len: bool,
  idx: bool,
  minmatchlen: Option<i64>,
  withmatchlen: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(7);
    args.push(key1.into());
    args.push(key2.into());

    if len {
      args.push(static_val!(LEN));
    }
    if idx {
      args.push(static_val!(IDX));
    }
    if let Some(minmatchlen) = minmatchlen {
      args.push(static_val!(MINMATCHLEN));
      args.push(minmatchlen.into());
    }
    if withmatchlen {
      args.push(static_val!(WITHMATCHLEN));
    }

    Ok((CommandKind::Lcs, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
