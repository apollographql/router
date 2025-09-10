use super::*;
use crate::{
  error::Error,
  protocol::{
    command::{Command, CommandKind},
    hashers::ClusterHash,
    utils as protocol_utils,
  },
  types::{
    streams::{MultipleIDs, MultipleOrderedPairs, XCap, XPendingArgs, XID},
    Key,
    MultipleKeys,
    MultipleStrings,
    Value,
  },
  utils,
};
use bytes_utils::Str;
use std::convert::TryInto;

fn encode_cap(args: &mut Vec<Value>, cap: XCap) {
  if let Some((kind, trim, threshold, limit)) = cap.into_parts() {
    args.push(kind.to_str().into());
    args.push(trim.to_str().into());
    args.push(threshold.into_arg());
    if let Some(count) = limit {
      args.push(static_val!(LIMIT));
      args.push(count.into());
    }
  }
}

pub async fn xinfo_consumers<C: ClientLike>(client: &C, key: Key, groupname: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = vec![key.into(), groupname.into()];
    Ok((CommandKind::XinfoConsumers, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xinfo_groups<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || Ok((CommandKind::XinfoGroups, vec![key.into()]))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn xinfo_stream<C: ClientLike>(
  client: &C,
  key: Key,
  full: bool,
  count: Option<u64>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(4);
    args.push(key.into());

    if full {
      args.push(static_val!(FULL));
      if let Some(count) = count {
        args.push(static_val!(COUNT));
        args.push(count.try_into()?);
      }
    }

    Ok((CommandKind::XinfoStream, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xadd<C: ClientLike>(
  client: &C,
  key: Key,
  nomkstream: bool,
  cap: XCap,
  id: XID,
  fields: MultipleOrderedPairs,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(8 + (fields.len() * 2));
    args.push(key.into());

    if nomkstream {
      args.push(static_val!(NOMKSTREAM));
    }
    encode_cap(&mut args, cap);

    args.push(id.into_str().into());
    for (key, value) in fields.inner().into_iter() {
      args.push(key.into());
      args.push(value);
    }

    Ok((CommandKind::Xadd, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xtrim<C: ClientLike>(client: &C, key: Key, cap: XCap) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(6);
    args.push(key.into());
    encode_cap(&mut args, cap);

    Ok((CommandKind::Xtrim, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xdel<C: ClientLike>(client: &C, key: Key, ids: MultipleStrings) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + ids.len());
    args.push(key.into());

    for id in ids.inner().into_iter() {
      args.push(id.into());
    }
    Ok((CommandKind::Xdel, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xrange<C: ClientLike>(
  client: &C,
  key: Key,
  start: Value,
  end: Value,
  count: Option<u64>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(5);
    args.push(key.into());
    args.push(start);
    args.push(end);

    if let Some(count) = count {
      args.push(static_val!(COUNT));
      args.push(count.try_into()?);
    }

    Ok((CommandKind::Xrange, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xrevrange<C: ClientLike>(
  client: &C,
  key: Key,
  end: Value,
  start: Value,
  count: Option<u64>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(5);
    args.push(key.into());
    args.push(end);
    args.push(start);

    if let Some(count) = count {
      args.push(static_val!(COUNT));
      args.push(count.try_into()?);
    }

    Ok((CommandKind::Xrevrange, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xlen<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Xlen, key.into()).await
}

pub async fn xread<C: ClientLike>(
  client: &C,
  count: Option<u64>,
  block: Option<u64>,
  keys: MultipleKeys,
  ids: MultipleIDs,
) -> Result<Value, Error> {
  let is_clustered = client.inner().config.server.is_clustered();
  let frame = utils::request_response(client, move || {
    let is_blocking = block.is_some();
    let mut hash_slot = None;
    let mut args = Vec::with_capacity(5 + keys.len() + ids.len());

    if let Some(count) = count {
      args.push(static_val!(COUNT));
      args.push(count.try_into()?);
    }
    if let Some(block) = block {
      args.push(static_val!(BLOCK));
      args.push(block.try_into()?);
    }
    args.push(static_val!(STREAMS));

    for (idx, key) in keys.inner().into_iter().enumerate() {
      // set the hash slot from the first key. if any other keys hash into another slot the server will return
      // CROSSSLOT error
      if is_clustered && idx == 0 {
        hash_slot = Some(ClusterHash::Offset(args.len()));
      }

      args.push(key.into());
    }
    for id in ids.inner().into_iter() {
      args.push(id.into_str().into());
    }

    let mut command: Command = (CommandKind::Xread, args).into();
    command.can_pipeline = !is_blocking;
    command.hasher = hash_slot.unwrap_or(ClusterHash::Random);
    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xgroup_create<C: ClientLike>(
  client: &C,
  key: Key,
  groupname: Str,
  id: XID,
  mkstream: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(4);
    args.push(key.into());
    args.push(groupname.into());
    args.push(id.into_str().into());
    if mkstream {
      args.push(static_val!(MKSTREAM));
    }

    Ok((CommandKind::Xgroupcreate, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xgroup_createconsumer<C: ClientLike>(
  client: &C,
  key: Key,
  groupname: Str,
  consumername: Str,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::XgroupCreateConsumer, vec![
      key.into(),
      groupname.into(),
      consumername.into(),
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xgroup_delconsumer<C: ClientLike>(
  client: &C,
  key: Key,
  groupname: Str,
  consumername: Str,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::XgroupDelConsumer, vec![
      key.into(),
      groupname.into(),
      consumername.into(),
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xgroup_destroy<C: ClientLike>(client: &C, key: Key, groupname: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::XgroupDestroy, vec![key.into(), groupname.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xgroup_setid<C: ClientLike>(client: &C, key: Key, groupname: Str, id: XID) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::XgroupSetId, vec![
      key.into(),
      groupname.into(),
      id.into_str().into(),
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xreadgroup<C: ClientLike>(
  client: &C,
  group: Str,
  consumer: Str,
  count: Option<u64>,
  block: Option<u64>,
  noack: bool,
  keys: MultipleKeys,
  ids: MultipleIDs,
) -> Result<Value, Error> {
  let is_clustered = client.inner().config.server.is_clustered();
  let frame = utils::request_response(client, move || {
    let is_blocking = block.is_some();
    let mut hash_slot = None;

    let mut args = Vec::with_capacity(9 + keys.len() + ids.len());
    args.push(static_val!(GROUP));
    args.push(group.into());
    args.push(consumer.into());

    if let Some(count) = count {
      args.push(static_val!(COUNT));
      args.push(count.try_into()?);
    }
    if let Some(block) = block {
      args.push(static_val!(BLOCK));
      args.push(block.try_into()?);
    }
    if noack {
      args.push(static_val!(NOACK));
    }

    args.push(static_val!(STREAMS));
    for (idx, key) in keys.inner().into_iter().enumerate() {
      if is_clustered && idx == 0 {
        hash_slot = Some(ClusterHash::Offset(args.len()));
      }

      args.push(key.into());
    }
    for id in ids.inner().into_iter() {
      args.push(id.into_str().into());
    }

    let mut command: Command = (CommandKind::Xreadgroup, args).into();
    command.can_pipeline = !is_blocking;
    command.hasher = hash_slot.unwrap_or(ClusterHash::Random);
    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xack<C: ClientLike>(client: &C, key: Key, group: Str, ids: MultipleIDs) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2 + ids.len());
    args.push(key.into());
    args.push(group.into());

    for id in ids.inner().into_iter() {
      args.push(id.into_str().into());
    }
    Ok((CommandKind::Xack, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xclaim<C: ClientLike>(
  client: &C,
  key: Key,
  group: Str,
  consumer: Str,
  min_idle_time: u64,
  ids: MultipleIDs,
  idle: Option<u64>,
  time: Option<u64>,
  retry_count: Option<u64>,
  force: bool,
  justid: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(12 + ids.len());
    args.push(key.into());
    args.push(group.into());
    args.push(consumer.into());
    args.push(min_idle_time.try_into()?);

    for id in ids.inner().into_iter() {
      args.push(id.into_str().into());
    }
    if let Some(idle) = idle {
      args.push(static_val!(IDLE));
      args.push(idle.try_into()?);
    }
    if let Some(time) = time {
      args.push(static_val!(TIME));
      args.push(time.try_into()?);
    }
    if let Some(retry_count) = retry_count {
      args.push(static_val!(RETRYCOUNT));
      args.push(retry_count.try_into()?);
    }
    if force {
      args.push(static_val!(FORCE));
    }
    if justid {
      args.push(static_val!(JUSTID));
    }

    Ok((CommandKind::Xclaim, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xautoclaim<C: ClientLike>(
  client: &C,
  key: Key,
  group: Str,
  consumer: Str,
  min_idle_time: u64,
  start: XID,
  count: Option<u64>,
  justid: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(8);
    args.push(key.into());
    args.push(group.into());
    args.push(consumer.into());
    args.push(min_idle_time.try_into()?);
    args.push(start.into_str().into());

    if let Some(count) = count {
      args.push(static_val!(COUNT));
      args.push(count.try_into()?);
    }
    if justid {
      args.push(static_val!(JUSTID));
    }

    Ok((CommandKind::Xautoclaim, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn xpending<C: ClientLike>(
  client: &C,
  key: Key,
  group: Str,
  cmd_args: XPendingArgs,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(8);
    args.push(key.into());
    args.push(group.into());

    if let Some((idle, start, end, count, consumer)) = cmd_args.into_parts()? {
      if let Some(idle) = idle {
        args.push(static_val!(IDLE));
        args.push(idle.try_into()?);
      }
      args.push(start.into_str().into());
      args.push(end.into_str().into());
      args.push(count.try_into()?);
      if let Some(consumer) = consumer {
        args.push(consumer.into());
      }
    }

    Ok((CommandKind::Xpending, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
