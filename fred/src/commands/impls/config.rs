use super::*;
use crate::{protocol::command::CommandKind, types::*};
use bytes_utils::Str;

ok_cmd!(config_resetstat, ConfigResetStat);
ok_cmd!(config_rewrite, ConfigRewrite);

pub async fn config_get<C: ClientLike>(client: &C, parameter: Str) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::ConfigGet, parameter.into()).await
}

pub async fn config_set<C: ClientLike>(client: &C, parameter: Str, value: Value) -> Result<(), Error> {
  args_ok_cmd(client, CommandKind::ConfigSet, vec![parameter.into(), value]).await
}
