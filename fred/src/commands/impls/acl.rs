use super::*;
use crate::{
  protocol::{command::CommandKind, utils as protocol_utils},
  types::*,
  utils,
};
use bytes_utils::Str;

ok_cmd!(acl_load, AclLoad);
ok_cmd!(acl_save, AclSave);
values_cmd!(acl_list, AclList);
values_cmd!(acl_users, AclUsers);
value_cmd!(acl_whoami, AclWhoAmI);

pub async fn acl_setuser<C: ClientLike>(client: &C, username: Str, rules: MultipleValues) -> Result<(), Error> {
  let frame = utils::request_response(client, move || {
    let rules = rules.into_multiple_values();
    let mut args = Vec::with_capacity(rules.len() + 1);
    args.push(username.into());

    for rule in rules.into_iter() {
      args.push(rule);
    }
    Ok((CommandKind::AclSetUser, args))
  })
  .await?;

  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

pub async fn acl_getuser<C: ClientLike>(client: &C, username: Value) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::AclGetUser, username).await
}

pub async fn acl_deluser<C: ClientLike>(client: &C, usernames: MultipleKeys) -> Result<Value, Error> {
  let args: Vec<Value> = usernames.inner().into_iter().map(|k| k.into()).collect();
  let frame = utils::request_response(client, move || Ok((CommandKind::AclDelUser, args))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn acl_cat<C: ClientLike>(client: &C, category: Option<Str>) -> Result<Value, Error> {
  let args: Vec<Value> = if let Some(cat) = category {
    vec![cat.into()]
  } else {
    Vec::new()
  };

  let frame = utils::request_response(client, move || Ok((CommandKind::AclCat, args))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn acl_genpass<C: ClientLike>(client: &C, bits: Option<u16>) -> Result<Value, Error> {
  let args: Vec<Value> = if let Some(bits) = bits {
    vec![bits.into()]
  } else {
    Vec::new()
  };

  let frame = utils::request_response(client, move || Ok((CommandKind::AclGenPass, args))).await?;
  protocol_utils::frame_to_results(frame)
}

pub async fn acl_log_reset<C: ClientLike>(client: &C) -> Result<(), Error> {
  let frame = utils::request_response(client, || Ok((CommandKind::AclLog, vec![static_val!(RESET)]))).await?;
  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

pub async fn acl_log_count<C: ClientLike>(client: &C, count: Option<u32>) -> Result<Value, Error> {
  let args: Vec<Value> = if let Some(count) = count {
    vec![count.into()]
  } else {
    Vec::new()
  };

  let frame = utils::request_response(client, move || Ok((CommandKind::AclLog, args))).await?;
  protocol_utils::frame_to_results(frame)
}
