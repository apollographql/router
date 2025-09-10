#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::prelude::*;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Error> {
  pretty_env_logger::init();

  let publisher_client = Client::default();
  let subscriber_client = Client::default();
  publisher_client.init().await?;
  subscriber_client.init().await?;

  let _subscriber_jh = tokio::spawn(async move {
    loop {
      let (key, value): (String, i64) = match subscriber_client.blpop("foo", 5.0).await.ok() {
        Some(value) => value,
        None => continue,
      };

      println!("BLPOP result on {}: {}", key, value);
    }
  });

  for idx in 0 .. 30 {
    let _: () = publisher_client.rpush("foo", idx).await?;
    sleep(Duration::from_secs(1)).await;
  }

  Ok(())
}
