#![allow(unused_imports)]
use fred::{error::Error, interfaces::*, prelude::Client, types::config::Config};

#[cfg(all(feature = "i-cluster", feature = "i-client"))]
pub async fn should_use_each_cluster_node(client: Client, _: Config) -> Result<(), Error> {
  let connections = client.active_connections();

  let mut servers = Vec::new();
  for server in connections.iter() {
    let server_addr = client
      .with_cluster_node(server)
      .client_info::<String>()
      .await?
      .split(' ')
      .find_map(|s| {
        let parts: Vec<&str> = s.split('=').collect();
        if parts[0] == "laddr" {
          Some(parts[1].to_owned())
        } else {
          None
        }
      })
      .expect("Failed to read or parse client info.");

    assert_eq!(server_addr, format!("{}", server));
    servers.push(server_addr);
  }

  assert_eq!(servers.len(), connections.len());
  Ok(())
}
