use crate::{
  error::{Error, ErrorKind},
  interfaces::ClientLike,
  protocol::{
    command::{Command, CommandKind},
    responders::ResponseKind,
    utils as protocol_utils,
  },
  runtime::oneshot_channel,
  types::{client::Toggle, ClusterHash, MultipleStrings, Value},
  utils,
};
use redis_protocol::redis_keyslot;

pub static PREFIX: &str = "PREFIX";
pub static REDIRECT: &str = "REDIRECT";
pub static BCAST: &str = "BCAST";
pub static OPTIN: &str = "OPTIN";
pub static OPTOUT: &str = "OPTOUT";
pub static NOLOOP: &str = "NOLOOP";
pub static YES: &str = "YES";
pub static NO: &str = "NO";

fn tracking_args(
  toggle: Toggle,
  redirect: Option<i64>,
  prefixes: MultipleStrings,
  bcast: bool,
  optin: bool,
  optout: bool,
  noloop: bool,
) -> Vec<Value> {
  let mut args = Vec::with_capacity(prefixes.len() * 2 + 7);
  args.push(static_val!(toggle.to_str()));
  if let Some(redirect) = redirect {
    args.push(static_val!(REDIRECT));
    args.push(redirect.into());
  }
  for prefix in prefixes.inner().into_iter() {
    args.push(static_val!(PREFIX));
    args.push(prefix.into());
  }
  if bcast {
    args.push(static_val!(BCAST));
  }
  if optin {
    args.push(static_val!(OPTIN));
  }
  if optout {
    args.push(static_val!(OPTOUT));
  }
  if noloop {
    args.push(static_val!(NOLOOP));
  }

  args
}

pub async fn start_tracking<C: ClientLike>(
  client: &C,
  prefixes: MultipleStrings,
  bcast: bool,
  optin: bool,
  optout: bool,
  noloop: bool,
) -> Result<(), Error> {
  if !client.inner().is_resp3() {
    return Err(Error::new(ErrorKind::Config, "Client tracking requires RESP3."));
  }

  let args = tracking_args(Toggle::On, None, prefixes, bcast, optin, optout, noloop);
  if client.inner().config.server.is_clustered() {
    if bcast {
      // only send the tracking command on one connection when in bcast mode
      let frame = utils::request_response(client, move || {
        let mut command = Command::new(CommandKind::ClientTracking, args);
        command.hasher = ClusterHash::Custom(redis_keyslot(client.id().as_bytes()));
        Ok(command)
      })
      .await?;

      protocol_utils::frame_to_results(frame)?.convert()
    } else {
      // send the tracking command to all nodes when not in bcast mode
      let (tx, rx) = oneshot_channel();
      let response = ResponseKind::Respond(Some(tx));
      let command: Command = (CommandKind::_ClientTrackingCluster, args, response).into();
      client.send_command(command)?;

      let frame = utils::timeout(rx, client.inner().internal_command_timeout()).await??;
      let _ = protocol_utils::frame_to_results(frame)?;
      Ok(())
    }
  } else {
    utils::request_response(client, move || Ok((CommandKind::ClientTracking, args)))
      .await
      .and_then(protocol_utils::frame_to_results)
      .and_then(|v| v.convert())
  }
}

pub async fn stop_tracking<C: ClientLike>(client: &C) -> Result<(), Error> {
  if !client.inner().is_resp3() {
    return Err(Error::new(ErrorKind::Config, "Client tracking requires RESP3."));
  }

  let args = vec![static_val!(Toggle::Off.to_str())];
  if client.is_clustered() {
    // turn off tracking on all connections
    let (tx, rx) = oneshot_channel();
    let response = ResponseKind::Respond(Some(tx));
    let command: Command = (CommandKind::_ClientTrackingCluster, args, response).into();
    client.send_command(command)?;

    let frame = utils::timeout(rx, client.inner().internal_command_timeout()).await??;
    let _ = protocol_utils::frame_to_results(frame)?;
    Ok(())
  } else {
    utils::request_response(client, move || Ok((CommandKind::ClientTracking, args)))
      .await
      .and_then(protocol_utils::frame_to_results)
      .and_then(|v| v.convert())
  }
}

pub async fn client_tracking<C: ClientLike>(
  client: &C,
  toggle: Toggle,
  redirect: Option<i64>,
  prefixes: MultipleStrings,
  bcast: bool,
  optin: bool,
  optout: bool,
  noloop: bool,
) -> Result<Value, Error> {
  let args = tracking_args(toggle, redirect, prefixes, bcast, optin, optout, noloop);

  utils::request_response(client, move || Ok((CommandKind::ClientTracking, args)))
    .await
    .and_then(protocol_utils::frame_to_results)
}

pub async fn client_trackinginfo<C: ClientLike>(client: &C) -> Result<Value, Error> {
  utils::request_response(client, move || Ok((CommandKind::ClientTrackingInfo, vec![])))
    .await
    .and_then(protocol_utils::frame_to_results)
}

pub async fn client_getredir<C: ClientLike>(client: &C) -> Result<Value, Error> {
  utils::request_response(client, move || Ok((CommandKind::ClientGetRedir, vec![])))
    .await
    .and_then(protocol_utils::frame_to_results)
}

pub async fn client_caching<C: ClientLike>(client: &C, enabled: bool) -> Result<Value, Error> {
  let args = if enabled {
    vec![static_val!(YES)]
  } else {
    vec![static_val!(NO)]
  };

  utils::request_response(client, move || Ok((CommandKind::ClientCaching, args)))
    .await
    .and_then(protocol_utils::frame_to_results)
}
