use fred::{
  clients::Client,
  error::Error,
  interfaces::*,
  types::{config::Config, Value},
};

pub async fn should_run_get_set_trx(client: Client, _config: Config) -> Result<(), Error> {
  let trx = client.multi();

  let _: () = trx.set("foo", "bar", None, None, false).await?;
  let _: () = trx.get("foo").await?;
  let results: Vec<String> = trx.exec(true).await?;

  assert_eq!(results, vec!["OK", "bar"]);
  Ok(())
}

pub async fn should_run_error_get_set_trx(client: Client, _config: Config) -> Result<(), Error> {
  let _: () = client.set("foo", "bar", None, None, false).await?;

  let trx = client.multi();
  let _: () = trx.incr("foo").await?;
  let _: Vec<Value> = trx.exec(true).await?;

  Ok(())
}

pub async fn should_fail_with_hashslot_error(client: Client, _config: Config) -> Result<(), Error> {
  let trx = client.multi();
  let _: () = trx.set("foo", "bar", None, None, false).await?;
  let _: () = trx.set("bar", "baz", None, None, false).await?;
  let _: Vec<Value> = trx.exec(true).await?;

  Ok(())
}
