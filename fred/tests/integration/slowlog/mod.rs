use fred::{prelude::*, types::SlowlogEntry};

pub async fn should_read_slowlog_length(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.slowlog_length().await?;
  // cant assert much here since the tests run in any order, and the call to reset the slowlog might run just before
  // this

  Ok(())
}

pub async fn should_read_slowlog_entries(client: Client, _: Config) -> Result<(), Error> {
  let entries: Vec<SlowlogEntry> = client.slowlog_get(Some(10)).await?;

  for entry in entries.into_iter() {
    assert!(!entry.duration.is_zero());
    assert!(entry.name.is_some());
  }

  Ok(())
}

pub async fn should_reset_slowlog(client: Client, _: Config) -> Result<(), Error> {
  client.slowlog_reset().await?;
  let len: i64 = client.slowlog_length().await?;
  // the slowlog length call might show up here
  assert!(len < 2);

  Ok(())
}
