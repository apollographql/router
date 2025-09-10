use fred::{cmd, prelude::*};
use std::time::Duration;
use tokio::time::sleep;

pub async fn should_flushall(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.custom(cmd!("SET"), vec!["foo{1}", "bar"]).await?;
  if client.is_clustered() {
    client.flushall_cluster().await?;
  } else {
    let _: () = client.flushall(false).await?;
  };

  let result: Option<String> = client.custom(cmd!("GET"), vec!["foo{1}"]).await?;
  assert!(result.is_none());

  Ok(())
}

pub async fn should_read_server_info(client: Client, _: Config) -> Result<(), Error> {
  let info: Option<String> = client.info(None).await?;
  assert!(info.is_some());

  Ok(())
}

pub async fn should_ping_pong_command(client: Client, _: Config) -> Result<(), Error> {
  let res: String = client.ping(None).await?;
  assert_eq!(res, "PONG");
  let res: String = client.ping(Some("hello world!".into())).await?;
  assert_eq!(res, "hello world!");
  Ok(())
}

pub async fn should_read_last_save(client: Client, _: Config) -> Result<(), Error> {
  let lastsave: Option<i64> = client.lastsave().await?;
  assert!(lastsave.is_some());

  Ok(())
}

pub async fn should_read_db_size(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. 50 {
    let _: () = client
      .custom(cmd!("SET"), vec![format!("foo-{}", idx), idx.to_string()])
      .await?;
  }

  // this is tricky to assert b/c the dbsize command isnt linked to a specific server in the cluster, hence the loop
  // above
  let db_size: i64 = client.dbsize().await?;
  assert!(db_size > 0);

  Ok(())
}

pub async fn should_start_bgsave(client: Client, _: Config) -> Result<(), Error> {
  let save_result: String = client.bgsave().await?;
  assert_eq!(save_result, "Background saving started");

  // need to ensure this finishes before it runs again or it'll return an error
  sleep(Duration::from_millis(1000)).await;
  Ok(())
}

pub async fn should_do_bgrewriteaof(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.bgrewriteaof().await?;
  // not much we can assert here aside from the command not failing

  // need to ensure this finishes before it runs again or it'll return an error
  sleep(Duration::from_millis(1000)).await;
  Ok(())
}

pub async fn should_select_index_command(client: Client, _: Config) -> Result<(), Error> {
  assert_eq!(client.select(0).await, Ok(()));
  Ok(())
}
