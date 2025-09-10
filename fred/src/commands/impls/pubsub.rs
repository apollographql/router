use super::*;
use crate::{
  protocol::{
    command::{Command, CommandKind},
    utils as protocol_utils,
  },
  types::*,
  utils,
};
use bytes_utils::Str;
use redis_protocol::redis_keyslot;

fn cluster_hash_legacy_command<C: ClientLike>(client: &C, command: &mut Command) {
  if client.is_clustered() {
    // send legacy (non-sharded) pubsub commands to the same node in a cluster so that `UNSUBSCRIBE` (without args)
    // works correctly. otherwise we'd have to send `UNSUBSCRIBE` to every node.
    let hash_slot = redis_keyslot(client.id().as_bytes());
    command.hasher = ClusterHash::Custom(hash_slot);
  }
}

pub async fn subscribe<C: ClientLike>(client: &C, channels: MultipleStrings) -> Result<(), Error> {
  let args = channels.inner().into_iter().map(|c| c.into()).collect();
  let mut command = Command::new(CommandKind::Subscribe, args);
  cluster_hash_legacy_command(client, &mut command);

  let frame = utils::request_response(client, move || Ok(command)).await?;
  protocol_utils::frame_to_results(frame).map(|_| ())
}

pub async fn unsubscribe<C: ClientLike>(client: &C, channels: MultipleStrings) -> Result<(), Error> {
  let args = channels.inner().into_iter().map(|c| c.into()).collect();
  let mut command = Command::new(CommandKind::Unsubscribe, args);
  cluster_hash_legacy_command(client, &mut command);

  let frame = utils::request_response(client, move || Ok(command)).await?;
  protocol_utils::frame_to_results(frame).map(|_| ())
}

pub async fn publish<C: ClientLike>(client: &C, channel: Str, message: Value) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Publish, vec![channel.into(), message]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn psubscribe<C: ClientLike>(client: &C, patterns: MultipleStrings) -> Result<(), Error> {
  let args = patterns.inner().into_iter().map(|c| c.into()).collect();
  let mut command = Command::new(CommandKind::Psubscribe, args);
  cluster_hash_legacy_command(client, &mut command);

  let frame = utils::request_response(client, move || Ok(command)).await?;
  protocol_utils::frame_to_results(frame).map(|_| ())
}

pub async fn punsubscribe<C: ClientLike>(client: &C, patterns: MultipleStrings) -> Result<(), Error> {
  let args = patterns.inner().into_iter().map(|c| c.into()).collect();
  let mut command = Command::new(CommandKind::Punsubscribe, args);
  cluster_hash_legacy_command(client, &mut command);

  let frame = utils::request_response(client, move || Ok(command)).await?;
  protocol_utils::frame_to_results(frame).map(|_| ())
}

pub async fn spublish<C: ClientLike>(client: &C, channel: Str, message: Value) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut command: Command = (CommandKind::Spublish, vec![channel.into(), message]).into();
    command.hasher = ClusterHash::FirstKey;

    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ssubscribe<C: ClientLike>(client: &C, channels: MultipleStrings) -> Result<(), Error> {
  let args = channels.inner().into_iter().map(|c| c.into()).collect();
  let mut command = Command::new(CommandKind::Ssubscribe, args);
  command.hasher = ClusterHash::FirstKey;

  let frame = utils::request_response(client, move || Ok(command)).await?;
  protocol_utils::frame_to_results(frame).map(|_| ())
}

pub async fn sunsubscribe<C: ClientLike>(client: &C, channels: MultipleStrings) -> Result<(), Error> {
  let args = channels.inner().into_iter().map(|c| c.into()).collect();
  let mut command = Command::new(CommandKind::Sunsubscribe, args);
  command.hasher = ClusterHash::FirstKey;

  let frame = utils::request_response(client, move || Ok(command)).await?;
  protocol_utils::frame_to_results(frame).map(|_| ())
}

pub async fn pubsub_channels<C: ClientLike>(client: &C, pattern: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, || {
    let args = if pattern.is_empty() {
      vec![]
    } else {
      vec![pattern.into()]
    };

    let mut command: Command = Command::new(CommandKind::PubsubChannels, args);
    cluster_hash_legacy_command(client, &mut command);

    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn pubsub_numpat<C: ClientLike>(client: &C) -> Result<Value, Error> {
  let frame = utils::request_response(client, || {
    let mut command: Command = Command::new(CommandKind::PubsubNumpat, vec![]);
    cluster_hash_legacy_command(client, &mut command);

    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn pubsub_numsub<C: ClientLike>(client: &C, channels: MultipleStrings) -> Result<Value, Error> {
  let frame = utils::request_response(client, || {
    let args: Vec<Value> = channels.inner().into_iter().map(|s| s.into()).collect();
    let mut command: Command = Command::new(CommandKind::PubsubNumsub, args);
    cluster_hash_legacy_command(client, &mut command);

    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn pubsub_shardchannels<C: ClientLike>(client: &C, pattern: Str) -> Result<Value, Error> {
  let frame =
    utils::request_response(client, || Ok((CommandKind::PubsubShardchannels, vec![pattern.into()]))).await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn pubsub_shardnumsub<C: ClientLike>(client: &C, channels: MultipleStrings) -> Result<Value, Error> {
  let frame = utils::request_response(client, || {
    let args: Vec<Value> = channels.inner().into_iter().map(|s| s.into()).collect();
    let has_args = !args.is_empty();
    let mut command: Command = Command::new(CommandKind::PubsubShardnumsub, args);
    if !has_args {
      cluster_hash_legacy_command(client, &mut command);
    }

    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
