#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::{monitor, prelude::*};
use futures::stream::StreamExt;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Error> {
  let monitor_jh = tokio::spawn(async move {
    let config = Config::default();
    let mut monitor_stream = monitor::run(config).await?;

    while let Some(command) = monitor_stream.next().await {
      // the Display implementation prints results in the same format as redis-cli
      println!("{}", command);
    }

    Ok::<(), Error>(())
  });

  let client = Client::default();
  client.init().await?;

  for idx in 0 .. 50 {
    let _: () = client.set("foo", idx, Some(Expiration::EX(10)), None, false).await?;
  }
  client.quit().await?;

  // wait a bit for the monitor stream to catch up
  sleep(Duration::from_secs(1)).await;
  monitor_jh.abort();
  Ok(())
}
