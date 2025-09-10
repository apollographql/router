#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::{interfaces::RedisJsonInterface, json_quote, prelude::*, util::NONE};
use serde_json::{json, Value};

// see the serde-json example for more information on deserializing responses
#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Builder::default_centralized().build()?;
  client.init().await?;

  // operate on objects
  let value = json!({
    "a": "b",
    "c": 1,
    "d": true
  });
  let _: () = client.json_set("foo", "$", value.clone(), None).await?;
  let result: Value = client.json_get("foo", NONE, NONE, NONE, "$").await?;
  assert_eq!(value, result[0]);
  let count: i64 = client.json_del("foo", "$..c").await?;
  assert_eq!(count, 1);
  let result: Value = client.json_get("foo", NONE, NONE, NONE, "$").await?;
  assert_eq!(result[0], json!({ "a": "b", "d": true }));

  // operate on arrays
  let _: () = client.json_set("foo", "$", json!(["a", "b"]), None).await?;
  let size: i64 = client
    .json_arrappend("foo", "$", vec![json_quote!("c"), json_quote!("d")])
    .await?;
  assert_eq!(size, 4);
  let size: i64 = client.json_arrappend("foo", "$", vec![json!({"e": "f"})]).await?;
  assert_eq!(size, 5);
  let len: i64 = client.json_arrlen("foo", NONE).await?;
  assert_eq!(len, 5);

  let result: Value = client.json_get("foo", NONE, NONE, NONE, "$").await?;
  assert_eq!(result[0], json!(["a", "b", "c", "d", { "e": "f" }]));

  // or see the redis-json integration tests for more
  client.quit().await?;
  Ok(())
}
