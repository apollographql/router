#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Error> {
  let pool = Builder::default_centralized().build_pool(5)?;
  pool.init().await?;

  // all client types, including `RedisPool`, implement the same command interface traits so callers can often use
  // them interchangeably. in this example each command below will be sent round-robin to the underlying 5 clients.
  assert!(pool.get::<Option<String>, _>("foo").await?.is_none());
  let _: () = pool.set("foo", "bar", None, None, false).await?;
  assert_eq!(pool.get::<String, _>("foo").await?, "bar");

  let _: () = pool.del("foo").await?;
  // interact with specific clients via next(), last(), or clients()
  let pipeline = pool.next().pipeline();
  let _: () = pipeline.incr("foo").await?;
  let _: () = pipeline.incr("foo").await?;
  assert_eq!(pipeline.last::<i64>().await?, 2);

  for client in pool.clients() {
    println!("{} connected to {:?}", client.id(), client.active_connections());
  }

  pool.quit().await?;
  Ok(())
}
