#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]
#![allow(clippy::mutable_key_type)]

use fred::{
  prelude::*,
  types::{
    config::{ClusterDiscoveryPolicy, ReplicaConfig},
    ClusterHash,
    RespVersion,
  },
  util::redis_keyslot,
};
use futures::future::try_join_all;
use log::info;
use std::collections::HashSet;

#[tokio::main]
async fn main() -> Result<(), Error> {
  pretty_env_logger::init();

  let config = Config::from_url("redis-cluster://foo:bar@redis-cluster-1:30001")?;
  let pool = Builder::from_config(config)
    .with_config(|config| {
      config.version = RespVersion::RESP3;
      config
        .server
        .set_cluster_discovery_policy(ClusterDiscoveryPolicy::ConfigEndpoint)
        .expect("Failed to set discovery policy.");
    })
    .with_connection_config(|config| {
      config.replica = ReplicaConfig {
        lazy_connections: true,
        primary_fallback: true,
        ..Default::default()
      };
    })
    .set_policy(ReconnectPolicy::new_exponential(0, 100, 30_000, 2))
    .build_pool(5)?;

  pool.init().await?;
  info!("Connected to redis.");
  lazy_connection_example(pool.next()).await?;

  // use pipelines and WAIT to concurrently SET then GET a value from replica nodes
  let mut ops = Vec::with_capacity(1000);
  for idx in 0 .. 1000 {
    let pool = pool.clone();
    ops.push(async move {
      let key: Key = format!("foo-{}", idx).into();
      let cluster_hash = ClusterHash::Custom(redis_keyslot(key.as_bytes()));

      // send WAIT to the cluster node that received SET
      let pipeline = pool.next().pipeline();
      let _: () = pipeline.set(&key, idx, None, None, false).await?;
      let _: () = pipeline
        .with_options(&Options {
          cluster_hash: Some(cluster_hash),
          ..Default::default()
        })
        .wait(1, 10_000)
        .await?;
      let _: () = pipeline.all().await?;

      assert_eq!(pool.replicas().get::<i64, _>(&key).await?, idx);
      Ok::<_, Error>(())
    });
  }
  try_join_all(ops).await?;

  Ok(())
}

// use one client to demonstrate how lazy connections are created. in this case each primary node is expected to have
// one replica.
async fn lazy_connection_example(client: &Client) -> Result<(), Error> {
  let replica_routing = client.replicas().nodes();
  let cluster_routing = client
    .cached_cluster_state()
    .expect("Failed to read cached cluster state.");
  let expected_primary = cluster_routing
    .get_server(redis_keyslot(b"foo"))
    .expect("Failed to read primary node owner for 'foo'");
  let old_connections: HashSet<_> = client.active_connections().into_iter().collect();

  // if `lazy_connections: true` the client creates the connection here
  let _: () = client.replicas().get("foo").await?;
  let new_connections: HashSet<_> = client.active_connections().into_iter().collect();
  let new_servers: Vec<_> = new_connections.difference(&old_connections).collect();
  // verify that 1 new connection was created, and that it's in the replica map as a replica of the expected primary
  // node
  assert_eq!(new_servers.len(), 1);
  assert_eq!(replica_routing.get(new_servers[0]), Some(expected_primary));

  // update the replica routing table and reset replica connections
  client.replicas().sync(true).await?;
  assert_eq!(old_connections.len(), client.active_connections().len());

  Ok(())
}
