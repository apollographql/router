use super::*;
use crate::{
  protocol::{command::CommandKind, utils as protocol_utils},
  types::*,
  utils,
};
use std::convert::TryInto;

pub async fn sadd<C: ClientLike>(client: &C, key: Key, members: MultipleValues) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let members = members.into_multiple_values();
    let mut args = Vec::with_capacity(1 + members.len());
    args.push(key.into());

    for member in members.into_iter() {
      args.push(member);
    }
    Ok((CommandKind::Sadd, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn scard<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Scard, key.into()).await
}

pub async fn sdiff<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(keys.len());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    Ok((CommandKind::Sdiff, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn sdiffstore<C: ClientLike>(client: &C, dest: Key, keys: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + keys.len());
    args.push(dest.into());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    Ok((CommandKind::Sdiffstore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn sinter<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(keys.len());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    Ok((CommandKind::Sinter, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn sinterstore<C: ClientLike>(client: &C, dest: Key, keys: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + keys.len());
    args.push(dest.into());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    Ok((CommandKind::Sinterstore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn sismember<C: ClientLike>(client: &C, key: Key, member: Value) -> Result<Value, Error> {
  args_value_cmd(client, CommandKind::Sismember, vec![key.into(), member]).await
}

pub async fn smismember<C: ClientLike>(client: &C, key: Key, members: MultipleValues) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let members = members.into_multiple_values();
    let mut args = Vec::with_capacity(1 + members.len());
    args.push(key.into());

    for member in members.into_iter() {
      args.push(member);
    }
    Ok((CommandKind::Smismember, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn smembers<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::Smembers, key.into()).await
}

pub async fn smove<C: ClientLike>(client: &C, source: Key, dest: Key, member: Value) -> Result<Value, Error> {
  let args = vec![source.into(), dest.into(), member];
  args_value_cmd(client, CommandKind::Smove, args).await
}

pub async fn spop<C: ClientLike>(client: &C, key: Key, count: Option<usize>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2);
    args.push(key.into());

    if let Some(count) = count {
      args.push(count.try_into()?);
    }
    Ok((CommandKind::Spop, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn srandmember<C: ClientLike>(client: &C, key: Key, count: Option<usize>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2);
    args.push(key.into());

    if let Some(count) = count {
      args.push(count.try_into()?);
    }
    Ok((CommandKind::Srandmember, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn srem<C: ClientLike>(client: &C, key: Key, members: MultipleValues) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let members = members.into_multiple_values();
    let mut args = Vec::with_capacity(1 + members.len());
    args.push(key.into());

    for member in members.into_iter() {
      args.push(member);
    }
    Ok((CommandKind::Srem, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn sunion<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(keys.len());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    Ok((CommandKind::Sunion, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn sunionstore<C: ClientLike>(client: &C, dest: Key, keys: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + keys.len());
    args.push(dest.into());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    Ok((CommandKind::Sunionstore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
