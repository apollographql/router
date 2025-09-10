#![allow(unused_macros)]
#![allow(dead_code)]

use crate::{
  error::Error,
  interfaces::ClientLike,
  protocol::{command::CommandKind, utils as protocol_utils},
  types::Value,
  utils,
};

pub static MATCH: &str = "MATCH";
pub static COUNT: &str = "COUNT";
pub static TYPE: &str = "TYPE";
#[cfg(any(feature = "i-geo", feature = "i-sorted-sets"))]
pub static CHANGED: &str = "CH";
#[cfg(any(feature = "i-lists", feature = "i-sorted-sets", feature = "i-streams"))]
pub static LIMIT: &str = "LIMIT";
pub static GET: &str = "GET";
pub static RESET: &str = "RESET";
pub static TO: &str = "TO";
pub static FORCE: &str = "FORCE";
pub static ABORT: &str = "ABORT";
pub static TIMEOUT: &str = "TIMEOUT";
pub static LEN: &str = "LEN";
pub static DB: &str = "DB";
pub static REPLACE: &str = "REPLACE";
pub static ID: &str = "ID";
pub static ANY: &str = "ANY";
pub static STORE: &str = "STORE";
pub static WITH_VALUES: &str = "WITHVALUES";
pub static SYNC: &str = "SYNC";
pub static ASYNC: &str = "ASYNC";
pub static RANK: &str = "RANK";
pub static MAXLEN: &str = "MAXLEN";
pub static REV: &str = "REV";
pub static ABSTTL: &str = "ABSTTL";
pub static IDLE_TIME: &str = "IDLETIME";
pub static FREQ: &str = "FREQ";
pub static FULL: &str = "FULL";
pub static NOMKSTREAM: &str = "NOMKSTREAM";
pub static MINID: &str = "MINID";
pub static BLOCK: &str = "BLOCK";
pub static STREAMS: &str = "STREAMS";
pub static MKSTREAM: &str = "MKSTREAM";
pub static GROUP: &str = "GROUP";
pub static NOACK: &str = "NOACK";
pub static IDLE: &str = "IDLE";
pub static TIME: &str = "TIME";
pub static RETRYCOUNT: &str = "RETRYCOUNT";
pub static JUSTID: &str = "JUSTID";
pub static SAMPLES: &str = "SAMPLES";
pub static LIBRARYNAME: &str = "LIBRARYNAME";
pub static WITHCODE: &str = "WITHCODE";
pub static IDX: &str = "IDX";
pub static MINMATCHLEN: &str = "MINMATCHLEN";
pub static WITHMATCHLEN: &str = "WITHMATCHLEN";

/// Macro to generate a command function that takes no arguments and expects an OK response - returning `()` to the
/// caller.
macro_rules! ok_cmd(
  ($name:ident, $cmd:tt) => {
    pub async fn $name<C: ClientLike>(client: &C) -> Result<(), Error> {
      let frame = crate::utils::request_response(client, || Ok((CommandKind::$cmd, vec![]))).await?;
      let response = crate::protocol::utils::frame_to_results(frame)?;
      crate::protocol::utils::expect_ok(&response)
    }
  }
);

/// Macro to generate a command function that takes no arguments and returns a single `Value` to the caller.
macro_rules! simple_cmd(
  ($name:ident, $cmd:tt, $res:ty) => {
    pub async fn $name<C: ClientLike>(client: &C) -> Result<$res, Error> {
      let frame = crate::utils::request_response(client, || Ok((CommandKind::$cmd, vec![]))).await?;
      crate::protocol::utils::frame_to_results(frame)
    }
  }
);

/// Macro to generate a command function that takes no arguments and returns a single `Value` to the caller.
macro_rules! value_cmd(
  ($name:ident, $cmd:tt) => {
    simple_cmd!($name, $cmd, Value);
  }
);

/// Macro to generate a command function that takes no arguments and returns a potentially nested `Value` to the
/// caller.
macro_rules! values_cmd(
  ($name:ident, $cmd:tt) => {
    pub async fn $name<C: ClientLike>(client: &C) -> Result<Value, Error> {
      let frame = crate::utils::request_response(client, || Ok((CommandKind::$cmd, vec![]))).await?;
      crate::protocol::utils::frame_to_results(frame)
    }
  }
);

/// A function that issues a command that only takes one argument and returns a single `Value`.
pub async fn one_arg_value_cmd<C: ClientLike>(client: &C, kind: CommandKind, arg: Value) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || Ok((kind, vec![arg]))).await?;
  protocol_utils::frame_to_results(frame)
}

/// A function that issues a command that only takes one argument and returns a potentially nested `Value`.
pub async fn one_arg_values_cmd<C: ClientLike>(client: &C, kind: CommandKind, arg: Value) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || Ok((kind, vec![arg]))).await?;
  protocol_utils::frame_to_results(frame)
}

/// A function that issues a command that only takes one argument and expects an OK response - returning `()` to the
/// caller.
pub async fn one_arg_ok_cmd<C: ClientLike>(client: &C, kind: CommandKind, arg: Value) -> Result<(), Error> {
  let frame = utils::request_response(client, move || Ok((kind, vec![arg]))).await?;

  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

/// A function that issues a command that takes any number of arguments and returns a single `Value` to the
/// caller.
pub async fn args_value_cmd<C: ClientLike>(client: &C, kind: CommandKind, args: Vec<Value>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || Ok((kind, args))).await?;
  protocol_utils::frame_to_results(frame)
}

/// A function that issues a command that takes any number of arguments and returns a potentially nested `Value`
/// to the caller.
pub async fn args_values_cmd<C: ClientLike>(client: &C, kind: CommandKind, args: Vec<Value>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || Ok((kind, args))).await?;
  protocol_utils::frame_to_results(frame)
}

/// A function that issues a command that takes any number of arguments and expects an OK response - returning `()` to
/// the caller.
pub async fn args_ok_cmd<C: ClientLike>(client: &C, kind: CommandKind, args: Vec<Value>) -> Result<(), Error> {
  let frame = utils::request_response(client, move || Ok((kind, args))).await?;
  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)
}

#[cfg(feature = "i-acl")]
pub mod acl;
#[cfg(feature = "i-client")]
pub mod client;
#[cfg(feature = "i-cluster")]
pub mod cluster;
#[cfg(feature = "i-config")]
pub mod config;
#[cfg(feature = "i-geo")]
pub mod geo;
#[cfg(feature = "i-hashes")]
pub mod hashes;
#[cfg(feature = "i-hyperloglog")]
pub mod hyperloglog;
#[cfg(feature = "i-keys")]
pub mod keys;
#[cfg(feature = "i-lists")]
pub mod lists;
#[cfg(feature = "i-scripts")]
pub mod lua;
#[cfg(feature = "i-memory")]
pub mod memory;
#[cfg(feature = "i-pubsub")]
pub mod pubsub;
#[cfg(feature = "i-redis-json")]
pub mod redis_json;
#[cfg(feature = "i-redisearch")]
pub mod redisearch;
pub mod scan;
#[cfg(feature = "sentinel-client")]
pub mod sentinel;
pub mod server;
#[cfg(feature = "i-sets")]
pub mod sets;
#[cfg(feature = "i-slowlog")]
pub mod slowlog;
#[cfg(feature = "i-sorted-sets")]
pub mod sorted_sets;
#[cfg(feature = "i-streams")]
pub mod streams;
pub mod strings;
#[cfg(feature = "i-time-series")]
pub mod timeseries;
#[cfg(feature = "i-tracking")]
pub mod tracking;
