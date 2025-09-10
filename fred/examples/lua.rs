#![allow(clippy::disallowed_names)]

use fred::{
  prelude::*,
  types::scripts::{Library, Script},
  util as fred_utils,
};

static SCRIPT: &str = "return {KEYS[1],KEYS[2],ARGV[1],ARGV[2]}";

#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Client::default();
  client.init().await?;

  let hash = fred_utils::sha1_hash(SCRIPT);
  if !client.script_exists::<bool, _>(&hash).await? {
    let _: () = client.script_load(SCRIPT).await?;
  }

  let results: Value = client.evalsha(&hash, vec!["foo", "bar"], vec![1, 2]).await?;
  println!("Script result for {hash}: {results:?}");

  // or use `EVAL`
  let results: Value = client.eval(SCRIPT, vec!["foo", "bar"], vec![1, 2]).await?;
  println!("Script result: {results:?}");

  client.quit().await?;
  Ok(())
}

// or use the `Script` utility types
#[allow(dead_code)]
async fn scripts() -> Result<(), Error> {
  let client = Client::default();
  client.init().await?;

  let script = Script::from_lua(SCRIPT);
  script.load(&client).await?;
  let _result: Vec<Value> = script.evalsha(&client, vec!["foo", "bar"], vec![1, 2]).await?;
  // retry after calling SCRIPT LOAD, if needed
  let (key1, key2, arg1, arg2): (String, String, i64, i64) = script
    .evalsha_with_reload(&client, vec!["foo", "bar"], vec![1, 2])
    .await?;
  println!("Script result: [{key1}, {key2}, {arg1}, {arg2}]");

  Ok(())
}

// use the `Function` and `Library` utility types
#[allow(dead_code)]
async fn functions() -> Result<(), Error> {
  let client = Client::default();
  client.init().await?;

  let echo_lua = include_str!("../tests/scripts/lua/echo.lua");
  let lib = Library::from_code(&client, echo_lua).await?;
  let func = lib.functions().get("echo").expect("Failed to read echo function");

  let result: Vec<String> = func.fcall(&client, vec!["foo{1}", "bar{1}"], vec!["3", "4"]).await?;
  assert_eq!(result, vec!["foo{1}", "bar{1}", "3", "4"]);

  Ok(())
}
