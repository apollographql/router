use super::*;
use crate::{
  clients::Client,
  error::*,
  interfaces,
  modules::inner::ClientInner,
  prelude::{Resp3Frame, ServerConfig},
  protocol::{
    command::{Command, CommandKind, RouterCommand},
    responders::ResponseKind,
    utils as protocol_utils,
  },
  runtime::{oneshot_channel, RefCount},
  types::*,
  utils,
};
use bytes_utils::Str;

pub async fn quit<C: ClientLike>(client: &C) -> Result<(), Error> {
  let inner = client.inner().clone();
  {
    // break out early if the client is already closed from a prior call to quit
    if inner.command_rx.read().is_some() {
      // command_rx should be None if the client is running since it's owned by the routing task
      _warn!(inner, "Attempted to quit client that was already stopped.");
      return Ok(());
    }
  }

  _debug!(inner, "Closing Redis connection with Quit command.");
  let (tx, rx) = oneshot_channel();
  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::Quit, vec![], response).into();

  inner.set_client_state(ClientState::Disconnecting);
  inner.notifications.broadcast_close();
  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;
  let _ = utils::timeout(rx, timeout_dur).await??;
  inner
    .notifications
    .close_public_receivers(inner.with_perf_config(|c| c.broadcast_channel_capacity));
  inner.backchannel.check_and_disconnect(&inner, None).await;

  Ok(())
}

pub async fn shutdown<C: ClientLike>(client: &C, flags: Option<ShutdownFlags>) -> Result<(), Error> {
  let inner = client.inner().clone();
  _debug!(inner, "Shutting down server.");

  let args = if let Some(flags) = flags {
    vec![flags.to_str().into()]
  } else {
    Vec::new()
  };
  let (tx, rx) = oneshot_channel();
  let mut command: Command = if inner.config.server.is_clustered() {
    let response = ResponseKind::new_buffer(tx);
    (CommandKind::Shutdown, args, response).into()
  } else {
    let response = ResponseKind::Respond(Some(tx));
    (CommandKind::Shutdown, args, response).into()
  };
  inner.set_client_state(ClientState::Disconnecting);
  inner.notifications.broadcast_close();

  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;
  let _ = utils::timeout(rx, timeout_dur).await??;
  inner
    .notifications
    .close_public_receivers(inner.with_perf_config(|c| c.broadcast_channel_capacity));
  inner.backchannel.check_and_disconnect(&inner, None).await;

  Ok(())
}

/// Create a new client struct for each unique primary cluster node based on the cached cluster state.
pub fn split(inner: &RefCount<ClientInner>) -> Result<Vec<Client>, Error> {
  if !inner.config.server.is_clustered() {
    return Err(Error::new(ErrorKind::Config, "Expected clustered redis deployment."));
  }
  let servers = inner.with_cluster_state(|state| Ok(state.unique_primary_nodes()))?;
  _debug!(inner, "Unique primary nodes in split: {:?}", servers);

  Ok(
    servers
      .into_iter()
      .map(|server| {
        let mut config = inner.config.as_ref().clone();
        config.server = ServerConfig::Centralized { server };
        let perf = inner.performance_config();
        let policy = inner.reconnect_policy();
        let connection = inner.connection_config();

        Client::new(config, Some(perf), Some(connection), policy)
      })
      .collect(),
  )
}

pub async fn force_reconnection(inner: &RefCount<ClientInner>) -> Result<(), Error> {
  let (tx, rx) = oneshot_channel();
  let command = RouterCommand::Reconnect {
    server:                               None,
    force:                                true,
    tx:                                   Some(tx),
    #[cfg(feature = "replicas")]
    replica:                              false,
  };
  interfaces::send_to_router(inner, command)?;

  rx.await?.map(|_| ())
}

pub async fn flushall<C: ClientLike>(client: &C, r#async: bool) -> Result<Value, Error> {
  let args = if r#async { vec![static_val!(ASYNC)] } else { Vec::new() };
  let frame = utils::request_response(client, move || Ok((CommandKind::FlushAll, args))).await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn flushall_cluster<C: ClientLike>(client: &C) -> Result<(), Error> {
  if !client.inner().config.server.is_clustered() {
    return flushall(client, false).await.map(|_| ());
  }

  let (tx, rx) = oneshot_channel();
  let response = ResponseKind::Respond(Some(tx));
  let mut command: Command = (CommandKind::_FlushAllCluster, vec![], response).into();
  let timeout_dur = utils::prepare_command(client, &mut command);
  client.send_command(command)?;

  let _ = utils::timeout(rx, timeout_dur).await??;
  Ok(())
}

pub async fn ping<C: ClientLike>(client: &C, message: Option<String>) -> Result<Value, Error> {
  let mut args = Vec::with_capacity(1);
  if let Some(message) = message {
    args.push(message.into());
  }

  let frame = utils::request_response(client, || Ok((CommandKind::Ping, args))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn select<C: ClientLike>(client: &C, index: Value) -> Result<Value, Error> {
  let frame = utils::request_response(client, || Ok((CommandKind::Select, vec![index]))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn info<C: ClientLike>(client: &C, section: Option<InfoKind>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1);
    if let Some(section) = section {
      args.push(section.to_str().into());
    }

    Ok((CommandKind::Info, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn hello<C: ClientLike>(
  client: &C,
  version: RespVersion,
  auth: Option<(Str, Str)>,
  setname: Option<Str>,
) -> Result<(), Error> {
  let mut args = if let Some((username, password)) = auth {
    vec![username.into(), password.into()]
  } else {
    vec![]
  };
  if let Some(name) = setname {
    args.push(name.into());
  }

  if client.inner().config.server.is_clustered() {
    let (tx, rx) = oneshot_channel();
    let mut command: Command = CommandKind::_HelloAllCluster(version).into();
    command.response = ResponseKind::Respond(Some(tx));

    let timeout_dur = utils::prepare_command(client, &mut command);
    client.send_command(command)?;
    let _ = utils::timeout(rx, timeout_dur).await??;
    Ok(())
  } else {
    let frame = utils::request_response(client, move || Ok((CommandKind::_Hello(version), args))).await?;
    let _ = protocol_utils::frame_to_results(frame)?;
    Ok(())
  }
}

pub async fn auth<C: ClientLike>(client: &C, username: Option<String>, password: Str) -> Result<(), Error> {
  let mut args = Vec::with_capacity(2);
  if let Some(username) = username {
    args.push(username.into());
  }
  args.push(password.into());

  if client.inner().config.server.is_clustered() {
    let (tx, rx) = oneshot_channel();
    let response = ResponseKind::Respond(Some(tx));
    let mut command: Command = (CommandKind::_AuthAllCluster, args, response).into();

    let timeout_dur = utils::prepare_command(client, &mut command);
    client.send_command(command)?;
    let _ = utils::timeout(rx, timeout_dur).await??;
    Ok(())
  } else {
    let frame = utils::request_response(client, move || Ok((CommandKind::Auth, args))).await?;

    let response = protocol_utils::frame_to_results(frame)?;
    protocol_utils::expect_ok(&response)
  }
}

pub async fn custom<C: ClientLike>(client: &C, cmd: CustomCommand, args: Vec<Value>) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::_Custom(cmd), args).await
}

pub async fn custom_raw<C: ClientLike>(
  client: &C,
  cmd: CustomCommand,
  args: Vec<Value>,
) -> Result<Resp3Frame, Error> {
  utils::request_response(client, move || Ok((CommandKind::_Custom(cmd), args))).await
}

#[cfg(feature = "i-server")]
value_cmd!(dbsize, DBSize);
#[cfg(feature = "i-server")]
value_cmd!(bgrewriteaof, BgreWriteAof);
#[cfg(feature = "i-server")]
value_cmd!(bgsave, BgSave);

#[cfg(feature = "i-server")]
pub async fn failover<C: ClientLike>(
  client: &C,
  to: Option<(String, u16)>,
  force: bool,
  abort: bool,
  timeout: Option<u32>,
) -> Result<(), Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(7);
    if let Some((host, port)) = to {
      args.push(static_val!(TO));
      args.push(host.into());
      args.push(port.into());
    }
    if force {
      args.push(static_val!(FORCE));
    }
    if abort {
      args.push(static_val!(ABORT));
    }
    if let Some(timeout) = timeout {
      args.push(static_val!(TIMEOUT));
      args.push(timeout.into());
    }

    Ok((CommandKind::Failover, args))
  })
  .await?;

  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

#[cfg(feature = "i-server")]
value_cmd!(lastsave, LastSave);

#[cfg(feature = "i-server")]
pub async fn wait<C: ClientLike>(client: &C, numreplicas: i64, timeout: i64) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::Wait, vec![numreplicas.into(), timeout.into()]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
