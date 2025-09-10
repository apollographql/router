use super::*;
use crate::{
  protocol::{command::CommandKind, utils as protocol_utils},
  utils,
};

pub async fn slowlog_get<C: ClientLike>(client: &C, count: Option<i64>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2);
    args.push(static_val!(GET));

    if let Some(count) = count {
      args.push(count.into());
    }

    Ok((CommandKind::Slowlog, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn slowlog_length<C: ClientLike>(client: &C) -> Result<Value, Error> {
  let frame = utils::request_response(client, || Ok((CommandKind::Slowlog, vec![LEN.into()]))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn slowlog_reset<C: ClientLike>(client: &C) -> Result<(), Error> {
  args_ok_cmd(client, CommandKind::Slowlog, vec![static_val!(RESET)]).await
}
