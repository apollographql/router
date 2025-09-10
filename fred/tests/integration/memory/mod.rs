use fred::{cmd, prelude::*, types::MemoryStats};

pub async fn should_run_memory_doctor(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.memory_doctor().await?;
  Ok(())
}

pub async fn should_run_memory_malloc_stats(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.memory_malloc_stats().await?;
  Ok(())
}

pub async fn should_run_memory_purge(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.memory_purge().await?;
  Ok(())
}

pub async fn should_run_memory_stats(client: Client, _: Config) -> Result<(), Error> {
  let stats: MemoryStats = client.memory_stats().await?;
  assert!(stats.total_allocated > 0);

  Ok(())
}

pub async fn should_run_memory_usage(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.custom(cmd!("SET"), vec!["foo", "bar"]).await?;
  assert!(client.memory_usage::<u64, _>("foo", None).await? > 0);

  Ok(())
}
