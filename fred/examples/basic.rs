#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::prelude::*;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Error> {
  // create a config from a URL
  let config = Config::from_url("redis://username:password@foo.com:6379/1")?;
  // see the `Builder` interface for more information
  let client = Builder::from_config(config)
    .with_connection_config(|config| {
      config.connection_timeout = Duration::from_secs(5);
      config.tcp = TcpConfig {
        nodelay: Some(true),
        ..Default::default()
      };
    })
    .build()?;
  client.init().await?;
  // callers can manage the tokio task driving the connections
  let _connection_task = client.init().await?;

  // respond to out-of-band connection errors
  client.on_error(|(error, server)| async move {
    println!("{:?}: Connection error: {:?}", server, error);
    Ok(())
  });
  client.on_reconnect(|server| async move {
    println!("Reconnected to {}", server);
    Ok(())
  });

  // convert response types to most common rust types
  let foo: Option<String> = client.get("foo").await?;
  println!("Foo: {:?}", foo);

  let _: () = client
    .set("foo", "bar", Some(Expiration::EX(1)), Some(SetOptions::NX), false)
    .await?;

  // or use turbofish. the first type is always the response type.
  println!("Foo: {:?}", client.get::<Option<String>, _>("foo").await?);

  client.quit().await?;
  Ok(())
}
