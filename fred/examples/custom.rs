#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::{
  cmd,
  prelude::*,
  types::{ClusterHash, CustomCommand},
};
use std::convert::TryInto;

#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Builder::default_centralized().build()?;
  client.init().await?;
  let _: () = client.lpush("foo", vec![1, 2, 3]).await?;

  let result: Vec<String> = client.custom(cmd!("LRANGE"), vec!["foo", "0", "3"]).await?;
  println!("LRANGE Values: {:?}", result);

  // or customize routing and blocking parameters
  let _ = cmd!("LRANGE", blocking: false);
  let _ = cmd!("LRANGE", hash: ClusterHash::FirstKey);
  let _ = cmd!("LRANGE", hash: ClusterHash::FirstKey, blocking: false);
  // which is shorthand for
  let command = CustomCommand::new("LRANGE", ClusterHash::FirstKey, false);

  // or use `custom_raw` to operate on RESP3 frames
  let _result: Vec<i64> = client
    .custom_raw(command, vec!["foo", "0", "3"])
    .await
    .and_then(|frame| frame.try_into())
    .and_then(|value: Value| value.convert())?;
  Ok(())
}
