use super::*;
use crate::{
  protocol::{command::CommandKind, utils as protocol_utils},
  types::*,
  utils,
};

pub async fn pfadd<C: ClientLike>(client: &C, key: Key, elements: MultipleValues) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let elements = elements.into_multiple_values();
    let mut args = Vec::with_capacity(1 + elements.len());
    args.push(key.into());

    for element in elements.into_iter() {
      args.push(element);
    }
    Ok((CommandKind::Pfadd, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn pfcount<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, Error> {
  let args: Vec<Value> = keys.inner().into_iter().map(|k| k.into()).collect();
  args_value_cmd(client, CommandKind::Pfcount, args).await
}

pub async fn pfmerge<C: ClientLike>(client: &C, dest: Key, sources: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + sources.len());
    args.push(dest.into());

    for source in sources.inner().into_iter() {
      args.push(source.into());
    }
    Ok((CommandKind::Pfmerge, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
