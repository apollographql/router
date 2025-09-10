#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::prelude::*;
use futures::StreamExt;

// requires tokio_stream 0.1.3 or later
use tokio_stream::wrappers::BroadcastStream;

/// There are two interfaces for interacting with connection events on the `EventInterface`.
///
/// * The `on_*` functions are generally easier to use but require spawning a new tokio task. They also currently only
///   support synchronous functions.
/// * The `*_rx` functions are somewhat more complicated to use but allow the caller to control the underlying channel
///   receiver directly. Additionally, these functions do not require spawning a new tokio task.
///
/// See the source for `on_any` for an example of how one might handle multiple receivers in one task.
///
/// The best approach depends on how many tasks the caller is willing to create. The `setup_pool` function shows
/// how one might combine multiple receiver streams in a `RedisPool` to minimize the overhead of new tokio tasks for
/// each underlying client.
#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Builder::default_centralized().build()?;

  // use the on_* functions
  let _reconnect_task = client.on_reconnect(|server| async move {
    println!("Reconnected to {}", server);
    Ok(())
  });
  let _error_task = client.on_error(|error| async move {
    println!("Connection error: {:?}", error);
    Ok(())
  });

  // use the *_rx functions to do the same thing shown above. although not shown here, callers have more freedom to
  // reduce the number of spawned tokio tasks with this interface.
  let mut reconnect_rx = client.reconnect_rx();
  let _reconnect_task_2 = tokio::spawn(async move {
    while let Ok(server) = reconnect_rx.recv().await {
      println!("Reconnected to {}", server);
    }
  });

  let mut error_rx = client.error_rx();
  let _error_task_2 = tokio::spawn(async move {
    while let Ok(error) = error_rx.recv().await {
      println!("Connection error: {:?}", error);
    }
  });

  client.init().await?;

  // ...

  client.quit().await?;
  Ok(())
}

/// Shows how to combine multiple event streams from multiple clients into one tokio task.
#[allow(dead_code)]
async fn setup_pool() -> Result<(), Error> {
  let pool = Builder::default_centralized().build_pool(5)?;

  // `select_all` does most of the work here but requires that the channel receivers implement `Stream`. the
  // `tokio_stream::wrappers::BroadcastStream` wrapper can be used to do this.
  let error_rxs: Vec<_> = pool
    .clients()
    .iter()
    .map(|client| BroadcastStream::new(client.error_rx()))
    .collect();
  let reconnect_rxs: Vec<_> = pool
    .clients()
    .iter()
    .map(|client| BroadcastStream::new(client.reconnect_rx()))
    .collect();
  let mut error_rx = futures::stream::select_all(error_rxs);
  let mut reconnect_rx = futures::stream::select_all(reconnect_rxs);

  let _all_events_task = tokio::spawn(async move {
    loop {
      tokio::select! {
        Some(Ok(error)) = error_rx.next() => {
          println!("Error: {:?}", error);
        }
        Some(Ok(server)) = reconnect_rx.next() => {
          println!("Reconnected to {}", server);
        }
      }
    }
  });

  pool.init().await?;

  // ...

  pool.quit().await?;
  Ok(())
}
