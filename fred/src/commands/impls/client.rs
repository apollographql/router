use super::*;
use crate::{
  protocol::{
    command::{Command, CommandKind},
    utils as protocol_utils,
  },
  types::{client::*, ClientUnblockFlag, Key},
  utils,
};
use bytes_utils::Str;

value_cmd!(client_id, ClientID);
value_cmd!(client_info, ClientInfo);

pub async fn client_kill<C: ClientLike>(client: &C, filters: Vec<ClientKillFilter>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(filters.len() * 2);

    for filter in filters.into_iter() {
      let (field, value) = filter.to_str();
      args.push(field.into());
      args.push(value.into());
    }

    Ok((CommandKind::ClientKill, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn client_list<C: ClientLike>(
  client: &C,
  r#type: Option<ClientKillType>,
  ids: Option<Vec<String>>,
) -> Result<Value, Error> {
  let ids: Option<Vec<Key>> = ids.map(|ids| ids.into_iter().map(|id| id.into()).collect());
  let frame = utils::request_response(client, move || {
    let max_args = 2 + ids.as_ref().map(|i| i.len()).unwrap_or(0);
    let mut args = Vec::with_capacity(max_args);

    if let Some(kind) = r#type {
      args.push(static_val!(TYPE));
      args.push(kind.to_str().into());
    }
    if let Some(ids) = ids {
      if !ids.is_empty() {
        args.push(static_val!(ID));

        for id in ids.into_iter() {
          args.push(id.into());
        }
      }
    }

    Ok((CommandKind::ClientList, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn client_pause<C: ClientLike>(
  client: &C,
  timeout: i64,
  mode: Option<ClientPauseKind>,
) -> Result<(), Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2);
    args.push(timeout.into());

    if let Some(mode) = mode {
      args.push(mode.to_str().into());
    }

    Ok((CommandKind::ClientPause, args))
  })
  .await?;

  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

value_cmd!(client_getname, ClientGetName);

pub async fn client_setname<C: ClientLike>(client: &C, name: Str) -> Result<(), Error> {
  let frame = utils::request_response(client, move || Ok((CommandKind::ClientSetname, vec![name.into()]))).await?;
  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

ok_cmd!(client_unpause, ClientUnpause);

pub async fn client_reply<C: ClientLike>(client: &C, flag: ClientReplyFlag) -> Result<(), Error> {
  let frame = utils::request_response(client, move || {
    Ok((CommandKind::ClientReply, vec![flag.to_str().into()]))
  })
  .await?;

  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

pub async fn client_unblock<C: ClientLike>(
  client: &C,
  id: Value,
  flag: Option<ClientUnblockFlag>,
) -> Result<Value, Error> {
  let inner = client.inner();

  let mut args = Vec::with_capacity(2);
  args.push(id);
  if let Some(flag) = flag {
    args.push(flag.to_str().into());
  }
  let command = Command::new(CommandKind::ClientUnblock, args);

  let frame = utils::backchannel_request_response(inner, command, false).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn unblock_self<C: ClientLike>(client: &C, flag: Option<ClientUnblockFlag>) -> Result<(), Error> {
  let inner = client.inner();
  let flag = flag.unwrap_or(ClientUnblockFlag::Error);
  let result = utils::interrupt_blocked_connection(inner, flag).await;
  inner.backchannel.set_unblocked();
  result
}

pub async fn echo<C: ClientLike>(client: &C, message: Value) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Echo, message).await
}
