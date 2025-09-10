use fred::{
  clients::{Client, Pool},
  error::Error,
  interfaces::*,
  types::config::Config,
};

#[cfg(feature = "i-keys")]
use fred::types::{config::ReconnectPolicy, Builder};
#[cfg(feature = "i-keys")]
use futures::future::try_join_all;

async fn create_and_ping_pool(config: &Config, count: usize) -> Result<(), Error> {
  let pool = Pool::new(config.clone(), None, None, None, count)?;
  pool.init().await?;

  for client in pool.clients().iter() {
    let _: () = client.ping(None).await?;
  }

  let _: () = pool.ping(None).await?;
  let _: () = pool.quit().await?;
  Ok(())
}

pub async fn should_connect_and_ping_static_pool_single_conn(_: Client, config: Config) -> Result<(), Error> {
  create_and_ping_pool(&config, 1).await
}

pub async fn should_connect_and_ping_static_pool_two_conn(_: Client, config: Config) -> Result<(), Error> {
  create_and_ping_pool(&config, 2).await
}

#[cfg(feature = "i-keys")]
pub async fn should_incr_exclusive_pool(client: Client, config: Config) -> Result<(), Error> {
  let perf = client.perf_config();
  let policy = client
    .client_reconnect_policy()
    .unwrap_or(ReconnectPolicy::new_linear(0, 1000, 100));
  let connection = client.connection_config().clone();
  let pool = Builder::from_config(config)
    .set_performance_config(perf)
    .set_policy(policy)
    .set_connection_config(connection)
    .build_exclusive_pool(5)?;
  pool.init().await?;

  for _ in 0 .. 10 {
    let client = pool.acquire().await;
    let _: () = client.incr("foo").await?;
  }
  assert_eq!(client.get::<i64, _>("foo").await?, 10);
  let _: () = client.del("foo").await?;

  let mut fts = Vec::with_capacity(10);
  for _ in 0 .. 10 {
    let pool = pool.clone();
    fts.push(async move {
      let client = pool.acquire().await;
      client.incr::<i64, _>("foo").await
    });
  }
  try_join_all(fts).await?;
  assert_eq!(client.get::<i64, _>("foo").await?, 10);

  Ok(())
}

#[cfg(all(feature = "i-keys", feature = "transactions"))]
pub async fn should_watch_and_trx_exclusive_pool(client: Client, config: Config) -> Result<(), Error> {
  let perf = client.perf_config();
  let policy = client
    .client_reconnect_policy()
    .unwrap_or(ReconnectPolicy::new_linear(0, 1000, 100));
  let connection = client.connection_config().clone();
  let pool = Builder::from_config(config)
    .set_performance_config(perf)
    .set_policy(policy)
    .set_connection_config(connection)
    .build_exclusive_pool(5)?;
  pool.init().await?;

  let _: () = client.set("foo{1}", 1, None, None, false).await?;

  let results: Option<(i64, i64, i64)> = {
    let client = pool.acquire().await;

    client.watch("foo").await?;
    if let Some(1) = client.get::<Option<i64>, _>("foo{1}").await? {
      let trx = client.multi();
      let _: () = trx.incr("foo{1}").await?;
      let _: () = trx.incr("bar{1}").await?;
      let _: () = trx.incr("baz{1}").await?;
      Some(trx.exec(true).await?)
    } else {
      None
    }
  };
  assert_eq!(results, Some((2, 1, 1)));
  assert_eq!(client.get::<i64, _>("bar{1}").await?, 1);
  assert_eq!(client.get::<i64, _>("baz{1}").await?, 1);

  Ok(())
}
