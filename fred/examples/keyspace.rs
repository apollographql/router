#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::prelude::*;
use std::time::Duration;
use tokio::time::sleep;

/// Examples showing how to set up keyspace notifications with clustered or centralized/sentinel deployments.
///
/// The most complicated part of this process involves safely handling reconnections. Keyspace events rely on the
/// pubsub interface, and clients are required to subscribe or resubscribe whenever a new connection is created. These
/// examples show how to manually handle reconnections, but the caller can also use the `SubscriberClient` interface
/// to remove some of the boilerplate.
///
/// If callers do not need the keyspace subscriptions to survive reconnects then the process is more
/// straightforward.
///
/// Both examples assume that the server has been configured to emit keyspace events (via `notify-keyspace-events`).
#[tokio::main]
async fn main() -> Result<(), Error> {
  clustered_keyspace_events().await?;
  centralized_keyspace_events().await?;
  Ok(())
}

async fn fake_traffic(client: &Client, amount: usize) -> Result<(), Error> {
  // use a new client since the provided client is subscribed to keyspace events
  let client = client.clone_new();
  client.init().await?;

  for idx in 0 .. amount {
    let key: Key = format!("foo-{}", idx).into();

    let _: () = client.set(&key, 1, None, None, false).await?;
    let _: () = client.incr(&key).await?;
    let _: () = client.del(&key).await?;
  }

  client.quit().await?;
  Ok(())
}

async fn centralized_keyspace_events() -> Result<(), Error> {
  let subscriber = Builder::default_centralized().build()?;

  let reconnect_subscriber = subscriber.clone();
  // resubscribe to the foo- prefix whenever we reconnect to a server
  let _reconnect_task = tokio::spawn(async move {
    let mut reconnect_rx = reconnect_subscriber.reconnect_rx();

    while let Ok(server) = reconnect_rx.recv().await {
      println!("Reconnected to {}. Subscribing to keyspace events...", server);
      reconnect_subscriber.psubscribe("__key__*:foo*").await?;
    }

    Ok::<_, Error>(())
  });

  // connect after setting up the reconnection logic
  subscriber.init().await?;

  let mut keyspace_rx = subscriber.keyspace_event_rx();
  // set up a task that listens for keyspace events
  let _keyspace_task = tokio::spawn(async move {
    while let Ok(event) = keyspace_rx.recv().await {
      println!(
        "Recv: {} on {} in db {}",
        event.operation,
        event.key.as_str_lossy(),
        event.db
      );
    }

    Ok::<_, Error>(())
  });

  // generate fake traffic and wait a second
  fake_traffic(&subscriber, 1_000).await?;
  sleep(Duration::from_secs(1)).await;
  subscriber.quit().await?;
  Ok(())
}

async fn clustered_keyspace_events() -> Result<(), Error> {
  let subscriber = Builder::default_clustered().build()?;

  let reconnect_subscriber = subscriber.clone();
  // resubscribe to the foo- prefix whenever we reconnect to a server
  let _reconnect_task = tokio::spawn(async move {
    let mut reconnect_rx = reconnect_subscriber.reconnect_rx();

    // in 7.x the reconnection interface added a `Server` struct to reconnect events to make this easier.
    while let Ok(server) = reconnect_rx.recv().await {
      println!("Reconnected to {}. Subscribing to keyspace events...", server);
      reconnect_subscriber
        .with_cluster_node(server)
        .psubscribe("__key__*:foo*")
        .await?;
    }

    Ok::<_, Error>(())
  });

  // connect after setting up the reconnection logic
  subscriber.init().await?;

  let mut keyspace_rx = subscriber.keyspace_event_rx();
  // set up a task that listens for keyspace events
  let _keyspace_task = tokio::spawn(async move {
    while let Ok(event) = keyspace_rx.recv().await {
      println!(
        "Recv: {} on {} in db {}",
        event.operation,
        event.key.as_str_lossy(),
        event.db
      );
    }

    Ok::<_, Error>(())
  });

  // generate fake traffic and wait a second
  fake_traffic(&subscriber, 1_000).await?;
  sleep(Duration::from_secs(1)).await;
  subscriber.quit().await?;
  Ok(())
}
