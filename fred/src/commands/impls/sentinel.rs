use super::*;
use crate::{
  error::Error,
  protocol::{command::CommandKind, utils as protocol_utils},
  router::sentinel::{
    CKQUORUM,
    CONFIG,
    FAILOVER,
    FLUSHCONFIG,
    GET_MASTER_ADDR_BY_NAME,
    INFO_CACHE,
    MASTER,
    MASTERS,
    MONITOR,
    MYID,
    PENDING_SCRIPTS,
    REMOVE,
    REPLICAS,
    SENTINELS,
    SET,
    SIMULATE_FAILURE,
  },
  types::*,
  utils,
};
use bytes_utils::Str;
use std::net::IpAddr;

pub async fn config_get<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = vec![static_val!(CONFIG), static_val!(GET), name.into()];
    Ok((CommandKind::Sentinel, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn config_set<C: ClientLike>(client: &C, name: Str, value: Value) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![
      static_val!(CONFIG),
      static_val!(SET),
      name.into(),
      value,
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ckquorum<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![static_val!(CKQUORUM), name.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn flushconfig<C: ClientLike>(client: &C) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::Sentinel, vec![static_val!(FLUSHCONFIG)]).await
}

pub async fn failover<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![static_val!(FAILOVER), name.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn get_master_addr_by_name<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![
      static_val!(GET_MASTER_ADDR_BY_NAME),
      name.into(),
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn info_cache<C: ClientLike>(client: &C) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::Sentinel, vec![static_val!(INFO_CACHE)]).await
}

pub async fn masters<C: ClientLike>(client: &C) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::Sentinel, vec![static_val!(MASTERS)]).await
}

pub async fn master<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![static_val!(MASTER), name.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn monitor<C: ClientLike>(
  client: &C,
  name: Str,
  ip: IpAddr,
  port: u16,
  quorum: u32,
) -> Result<Value, Error> {
  let ip = ip.to_string();
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![
      static_val!(MONITOR),
      name.into(),
      ip.into(),
      port.into(),
      quorum.into(),
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn myid<C: ClientLike>(client: &C) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::Sentinel, vec![static_val!(MYID)]).await
}

pub async fn pending_scripts<C: ClientLike>(client: &C) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::Sentinel, vec![static_val!(PENDING_SCRIPTS)]).await
}

pub async fn remove<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![static_val!(REMOVE), name.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn replicas<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![static_val!(REPLICAS), name.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn sentinels<C: ClientLike>(client: &C, name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![static_val!(SENTINELS), name.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn set<C: ClientLike>(client: &C, name: Str, options: Map) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2 + options.len());
    args.push(static_val!(SET));
    args.push(name.into());

    for (key, value) in options.inner().into_iter() {
      args.push(key.into());
      args.push(value);
    }
    Ok((CommandKind::Sentinel, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn simulate_failure<C: ClientLike>(client: &C, kind: SentinelFailureKind) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![
      static_val!(SIMULATE_FAILURE),
      kind.to_str().into(),
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn reset<C: ClientLike>(client: &C, pattern: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Sentinel, vec![static_val!(RESET), pattern.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
