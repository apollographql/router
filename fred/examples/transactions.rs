#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Client::default();
  client.init().await?;

  // transactions are buffered in memory before calling `exec`
  let trx = client.multi();
  let result: Value = trx.get("foo").await?;
  assert!(result.is_queued());
  let result: Value = trx.set("foo", "bar", None, None, false).await?;
  assert!(result.is_queued());
  let result: Value = trx.get("foo").await?;
  assert!(result.is_queued());

  let values: (Option<String>, (), String) = trx.exec(true).await?;
  println!("Transaction results: {:?}", values);

  client.quit().await?;
  Ok(())
}
