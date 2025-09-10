use super::*;
#[cfg(feature = "sha-1")]
use crate::util::sha1_hash;
use crate::{
  error::*,
  modules::inner::ClientInner,
  protocol::{
    command::{Command, CommandKind},
    hashers::ClusterHash,
    responders::ResponseKind,
    utils as protocol_utils,
  },
  runtime::{oneshot_channel, RefCount},
  types::{
    scripts::{FnPolicy, ScriptDebugFlag},
    *,
  },
  utils,
};
use bytes::Bytes;
use bytes_utils::Str;
use redis_protocol::resp3::types::BytesFrame as Resp3Frame;
use std::{convert::TryInto, str};

/// Check that all the keys in an EVAL* command belong to the same server, returning a key slot that maps to that
/// server.
pub fn check_key_slot(inner: &RefCount<ClientInner>, keys: &[Key]) -> Result<Option<u16>, Error> {
  if inner.config.server.is_clustered() {
    inner.with_cluster_state(|state| {
      let (mut cmd_server, mut cmd_slot) = (None, None);
      for key in keys.iter() {
        let key_slot = redis_protocol::redis_keyslot(key.as_bytes());

        if let Some(server) = state.get_server(key_slot) {
          if let Some(ref cmd_server) = cmd_server {
            if cmd_server != server {
              return Err(Error::new(
                ErrorKind::Cluster,
                "All keys must belong to the same cluster node.",
              ));
            }
          } else {
            cmd_server = Some(server.clone());
            cmd_slot = Some(key_slot);
          }
        } else {
          return Err(Error::new(
            ErrorKind::Cluster,
            format!("Missing server for hash slot {}", key_slot),
          ));
        }
      }

      Ok(cmd_slot)
    })
  } else {
    Ok(None)
  }
}

pub async fn script_load<C: ClientLike>(client: &C, script: Str) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::ScriptLoad, script.into()).await
}

#[cfg(feature = "sha-1")]
pub async fn script_load_cluster<C: ClientLike>(client: &C, script: Str) -> Result<Value, Error> {
  if !client.inner().config.server.is_clustered() {
    return script_load(client, script).await;
  }
  let hash = sha1_hash(&script);

  let (tx, rx) = oneshot_channel();
  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::_ScriptLoadCluster, vec![script.into()], response).into();

  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;
  let _ = utils::timeout(rx, timeout_dur).await??;
  Ok(hash.into())
}

ok_cmd!(script_kill, ScriptKill);

pub async fn script_kill_cluster<C: ClientLike>(client: &C) -> Result<(), Error> {
  if !client.inner().config.server.is_clustered() {
    return script_kill(client).await;
  }

  let (tx, rx) = oneshot_channel();
  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::_ScriptKillCluster, vec![], response).into();

  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;
  let _ = utils::timeout(rx, timeout_dur).await??;
  Ok(())
}

pub async fn script_flush<C: ClientLike>(client: &C, r#async: bool) -> Result<(), Error> {
  let frame = utils::request_response(client, move || {
    let arg = static_val!(if r#async { ASYNC } else { SYNC });
    Ok((CommandKind::ScriptFlush, vec![arg]))
  })
  .await?;

  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

pub async fn script_flush_cluster<C: ClientLike>(client: &C, r#async: bool) -> Result<(), Error> {
  if !client.inner().config.server.is_clustered() {
    return script_flush(client, r#async).await;
  }

  let (tx, rx) = oneshot_channel();
  let arg = static_val!(if r#async { ASYNC } else { SYNC });
  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::_ScriptFlushCluster, vec![arg], response).into();

  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;

  let _ = utils::timeout(rx, timeout_dur).await??;
  Ok(())
}

pub async fn script_exists<C: ClientLike>(client: &C, hashes: MultipleStrings) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = hashes.inner().into_iter().map(|s| s.into()).collect();
    Ok((CommandKind::ScriptExists, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn script_debug<C: ClientLike>(client: &C, flag: ScriptDebugFlag) -> Result<(), Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::ScriptDebug, vec![flag.to_str().into()]))
  })
  .await?;

  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

pub async fn evalsha<C: ClientLike>(
  client: &C,
  hash: Str,
  keys: MultipleKeys,
  cmd_args: MultipleValues,
) -> Result<Value, Error> {
  let keys = keys.inner();
  let custom_key_slot = check_key_slot(client.inner(), &keys)?;

  let frame = utils::request_response(client, move || {
    let cmd_args = cmd_args.into_multiple_values();
    let mut args = Vec::with_capacity(2 + keys.len() + cmd_args.len());
    args.push(hash.into());
    args.push(keys.len().try_into()?);

    for key in keys.into_iter() {
      args.push(key.into());
    }
    for arg in cmd_args.into_iter() {
      args.push(arg);
    }

    let mut command: Command = (CommandKind::EvalSha, args).into();
    command.hasher = custom_key_slot.map(ClusterHash::Custom).unwrap_or(ClusterHash::Random);
    command.can_pipeline = false;
    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn eval<C: ClientLike>(
  client: &C,
  script: Str,
  keys: MultipleKeys,
  cmd_args: MultipleValues,
) -> Result<Value, Error> {
  let keys = keys.inner();
  let custom_key_slot = check_key_slot(client.inner(), &keys)?;

  let frame = utils::request_response(client, move || {
    let cmd_args = cmd_args.into_multiple_values();
    let mut args = Vec::with_capacity(2 + keys.len() + cmd_args.len());
    args.push(script.into());
    args.push(keys.len().try_into()?);

    for key in keys.into_iter() {
      args.push(key.into());
    }
    for arg in cmd_args.into_iter() {
      args.push(arg);
    }

    let mut command: Command = (CommandKind::Eval, args).into();
    command.hasher = custom_key_slot.map(ClusterHash::Custom).unwrap_or(ClusterHash::Random);
    command.can_pipeline = false;
    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn fcall<C: ClientLike>(
  client: &C,
  func: Str,
  keys: MultipleKeys,
  args: MultipleValues,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = args.into_multiple_values();
    let mut arguments = Vec::with_capacity(keys.len() + args.len() + 2);
    let mut custom_key_slot = None;

    arguments.push(func.into());
    arguments.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      custom_key_slot = Some(key.cluster_hash());
      arguments.push(key.into());
    }
    for arg in args.into_iter() {
      arguments.push(arg);
    }

    let mut command: Command = (CommandKind::Fcall, arguments).into();
    command.hasher = custom_key_slot.map(ClusterHash::Custom).unwrap_or(ClusterHash::Random);
    command.can_pipeline = false;
    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn fcall_ro<C: ClientLike>(
  client: &C,
  func: Str,
  keys: MultipleKeys,
  args: MultipleValues,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = args.into_multiple_values();
    let mut arguments = Vec::with_capacity(keys.len() + args.len() + 2);
    let mut custom_key_slot = None;

    arguments.push(func.into());
    arguments.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      custom_key_slot = Some(key.cluster_hash());
      arguments.push(key.into());
    }
    for arg in args.into_iter() {
      arguments.push(arg);
    }

    let mut command: Command = (CommandKind::FcallRO, arguments).into();
    command.hasher = custom_key_slot.map(ClusterHash::Custom).unwrap_or(ClusterHash::Random);
    command.can_pipeline = false;
    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn function_delete<C: ClientLike>(client: &C, library_name: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::FunctionDelete, vec![library_name.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn function_delete_cluster<C: ClientLike>(client: &C, library_name: Str) -> Result<(), Error> {
  if !client.inner().config.server.is_clustered() {
    return function_delete(client, library_name).await.map(|_| ());
  }

  let (tx, rx) = oneshot_channel();
  let args: Vec<Value> = vec![library_name.into()];

  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::_FunctionDeleteCluster, args, response).into();
  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;

  let _ = utils::timeout(rx, timeout_dur).await??;
  Ok(())
}

pub async fn function_flush<C: ClientLike>(client: &C, r#async: bool) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args = if r#async {
      vec![static_val!(ASYNC)]
    } else {
      vec![static_val!(SYNC)]
    };

    Ok((CommandKind::FunctionFlush, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn function_flush_cluster<C: ClientLike>(client: &C, r#async: bool) -> Result<(), Error> {
  if !client.inner().config.server.is_clustered() {
    return function_flush(client, r#async).await.map(|_| ());
  }

  let (tx, rx) = oneshot_channel();
  let args = if r#async {
    vec![static_val!(ASYNC)]
  } else {
    vec![static_val!(SYNC)]
  };

  let response = ResponseKind::Respond(Some(tx));
  let command: Command = (CommandKind::_FunctionFlushCluster, args, response).into();
  client.send_command(command)?;

  let _ = rx.await??;
  Ok(())
}

pub async fn function_kill<C: ClientLike>(client: &C) -> Result<Value, Error> {
  let inner = client.inner();
  let command = Command::new(CommandKind::FunctionKill, vec![]);

  let frame = utils::backchannel_request_response(inner, command, true).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn function_list<C: ClientLike>(
  client: &C,
  library_name: Option<Str>,
  withcode: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(3);

    if let Some(library_name) = library_name {
      args.push(static_val!(LIBRARYNAME));
      args.push(library_name.into());
    }
    if withcode {
      args.push(static_val!(WITHCODE));
    }

    Ok((CommandKind::FunctionList, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn function_load<C: ClientLike>(client: &C, replace: bool, code: Str) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2);
    if replace {
      args.push(static_val!(REPLACE));
    }
    args.push(code.into());

    Ok((CommandKind::FunctionLoad, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn function_load_cluster<C: ClientLike>(client: &C, replace: bool, code: Str) -> Result<Value, Error> {
  if !client.inner().config.server.is_clustered() {
    return function_load(client, replace, code).await;
  }

  let (tx, rx) = oneshot_channel();
  let mut args: Vec<Value> = Vec::with_capacity(2);
  if replace {
    args.push(static_val!(REPLACE));
  }
  args.push(code.into());

  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::_FunctionLoadCluster, args, response).into();
  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;

  // each value in the response array is the response from a different primary node
  match utils::timeout(rx, timeout_dur).await?? {
    Resp3Frame::Array { mut data, .. } => {
      if let Some(frame) = data.pop() {
        protocol_utils::frame_to_results(frame)
      } else {
        Err(Error::new(ErrorKind::Protocol, "Missing library name response frame."))
      }
    },
    Resp3Frame::SimpleError { data, .. } => Err(protocol_utils::pretty_error(&data)),
    Resp3Frame::BlobError { data, .. } => {
      let parsed = str::from_utf8(&data)?;
      Err(protocol_utils::pretty_error(parsed))
    },
    _ => Err(Error::new(ErrorKind::Protocol, "Invalid response type.")),
  }
}

pub async fn function_restore<C: ClientLike>(
  client: &C,
  serialized: Bytes,
  policy: FnPolicy,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2);
    args.push(serialized.into());
    args.push(policy.to_str().into());

    Ok((CommandKind::FunctionRestore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn function_restore_cluster<C: ClientLike>(
  client: &C,
  serialized: Bytes,
  policy: FnPolicy,
) -> Result<(), Error> {
  if !client.inner().config.server.is_clustered() {
    return function_restore(client, serialized, policy).await.map(|_| ());
  }

  let (tx, rx) = oneshot_channel();
  let args: Vec<Value> = vec![serialized.into(), policy.to_str().into()];

  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::_FunctionRestoreCluster, args, response).into();
  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;
  let _ = utils::timeout(rx, timeout_dur).await??;
  Ok(())
}

pub async fn function_stats<C: ClientLike>(client: &C) -> Result<Value, Error> {
  let inner = client.inner();
  let command = Command::new(CommandKind::FunctionStats, vec![]);

  let frame = utils::backchannel_request_response(inner, command, true).await?;
  protocol_utils::frame_to_results(frame)
}

value_cmd!(function_dump, FunctionDump);
