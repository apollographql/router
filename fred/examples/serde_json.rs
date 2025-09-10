#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]

use fred::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// from the serde json docs
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Person {
  name:   String,
  age:    u8,
  phones: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Client::default();
  client.init().await?;

  let value = json!({
    "foo": "a",
    "bar": "b"
  });
  let _: () = client.set("wibble", value.to_string(), None, None, false).await?;

  // converting back to a json `Value` will also try to parse nested json strings. if a value looks like json, but
  // cannot be parsed as json, then it will be returned as a string.
  assert_eq!(value, client.get::<Value, _>("wibble").await?);

  // or store types as json strings via Serialize and Deserialize
  let person = Person {
    name:   "Foo".into(),
    age:    42,
    phones: vec!["abc".into(), "123".into()],
  };

  let serialized = serde_json::to_string(&person)?;
  let _: () = client.set("foo", serialized, None, None, false).await?;
  // deserialize as a json value
  let person_json: Value = client.get("foo").await?;
  let deserialized: Person = serde_json::from_value(person_json)?;
  assert_eq!(person, deserialized);
  // or as a json string
  let person_string: String = client.get("foo").await?;
  let deserialized: Person = serde_json::from_str(&person_string)?;
  assert_eq!(person, deserialized);

  client.quit().await?;
  Ok(())
}
