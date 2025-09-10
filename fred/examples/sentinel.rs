#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::{prelude::*, types::config::Server};

#[tokio::main]
async fn main() -> Result<(), Error> {
  let config = Config {
    server: ServerConfig::Sentinel {
      // the name of the service, as configured in the sentinel configuration
      service_name:                               "my-service-name".into(),
      // the known host/port tuples for the sentinel nodes
      // the client will automatically update these if sentinels are added or removed
      hosts:                                      vec![
        Server::new("localhost", 26379),
        Server::new("localhost", 26380),
        Server::new("localhost", 26381),
      ],
      // callers can also use the `sentinel-auth` feature to use different credentials to sentinel nodes
      #[cfg(feature = "sentinel-auth")]
      username:                                   None,
      #[cfg(feature = "sentinel-auth")]
      password:                                   None,
    },
    // sentinels should use the same TLS settings as the Redis servers
    ..Default::default()
  };

  let client = Builder::from_config(config).build()?;
  client.init().await?;

  // ...

  client.quit().await?;
  Ok(())
}
