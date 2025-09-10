use crate::integration::utils;
use fred::{
  clients::Client,
  error::Error,
  interfaces::*,
  types::{config::Config, Value},
};
use std::{
  collections::{HashMap, HashSet},
  time::{SystemTime, UNIX_EPOCH},
};

fn assert_contains<T: Eq + PartialEq>(values: Vec<T>, item: &T) {
  for value in values.iter() {
    if value == item {
      return;
    }
  }

  panic!("Failed to find item in set.");
}

fn assert_diff_len(values: Vec<&'static str>, value: Value, len: usize) {
  if let Value::Array(items) = value {
    let mut expected = HashSet::with_capacity(values.len());
    for value in values.into_iter() {
      expected.insert(value.to_owned());
    }
    let mut actual = HashSet::with_capacity(items.len());
    for item in items.into_iter() {
      let s = &*item.as_str().unwrap();
      actual.insert(s.to_owned());
    }

    let diff = expected.difference(&actual).fold(0, |m, _| m + 1);
    assert_eq!(diff, len);
  } else {
    panic!("Expected value array");
  }
}

pub async fn should_hset_and_hget(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.hset("foo", ("a", 1)).await?;
  assert_eq!(result, 1);
  let result: i64 = client.hset("foo", vec![("b", 2), ("c", 3)]).await?;
  assert_eq!(result, 2);

  let a: i64 = client.hget("foo", "a").await?;
  assert_eq!(a, 1);
  let b: i64 = client.hget("foo", "b").await?;
  assert_eq!(b, 2);
  let c: i64 = client.hget("foo", "c").await?;
  assert_eq!(c, 3);

  Ok(())
}

pub async fn should_hset_and_hdel(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.hset("foo", vec![("a", 1), ("b", 2), ("c", 3)]).await?;
  assert_eq!(result, 3);
  let result: i64 = client.hdel("foo", vec!["a", "b"]).await?;
  assert_eq!(result, 2);
  let result: i64 = client.hdel("foo", "c").await?;
  assert_eq!(result, 1);
  let result: Option<i64> = client.hget("foo", "a").await?;
  assert!(result.is_none());

  Ok(())
}

pub async fn should_hexists(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hset("foo", ("a", 1)).await?;
  let a: bool = client.hexists("foo", "a").await?;
  assert!(a);
  let b: bool = client.hexists("foo", "b").await?;
  assert!(!b);

  Ok(())
}

pub async fn should_hgetall(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hset("foo", vec![("a", 1), ("b", 2), ("c", 3)]).await?;
  let values: HashMap<String, i64> = client.hgetall("foo").await?;

  assert_eq!(values.len(), 3);
  let mut expected = HashMap::new();
  expected.insert("a".into(), 1);
  expected.insert("b".into(), 2);
  expected.insert("c".into(), 3);
  assert_eq!(values, expected);

  Ok(())
}

pub async fn should_hincryby(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.hincrby("foo", "a", 1).await?;
  assert_eq!(result, 1);
  let result: i64 = client.hincrby("foo", "a", 2).await?;
  assert_eq!(result, 3);

  Ok(())
}

pub async fn should_hincryby_float(client: Client, _: Config) -> Result<(), Error> {
  let result: f64 = client.hincrbyfloat("foo", "a", 0.5).await?;
  assert_eq!(result, 0.5);
  let result: f64 = client.hincrbyfloat("foo", "a", 3.7).await?;
  assert_eq!(result, 4.2);

  Ok(())
}

pub async fn should_get_keys(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hset("foo", vec![("a", 1), ("b", 2), ("c", 3)]).await?;

  let keys = client.hkeys("foo").await?;
  assert_diff_len(vec!["a", "b", "c"], keys, 0);

  Ok(())
}

pub async fn should_hmset(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hmset("foo", vec![("a", 1), ("b", 2), ("c", 3)]).await?;

  let a: i64 = client.hget("foo", "a").await?;
  assert_eq!(a, 1);
  let b: i64 = client.hget("foo", "b").await?;
  assert_eq!(b, 2);
  let c: i64 = client.hget("foo", "c").await?;
  assert_eq!(c, 3);

  Ok(())
}

pub async fn should_hmget(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hmset("foo", vec![("a", 1), ("b", 2), ("c", 3)]).await?;

  let result: Vec<i64> = client.hmget("foo", vec!["a", "b"]).await?;
  assert_eq!(result, vec![1, 2]);

  Ok(())
}

pub async fn should_hsetnx(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hset("foo", ("a", 1)).await?;
  let result: bool = client.hsetnx("foo", "a", 2).await?;
  assert!(!result);
  let result: i64 = client.hget("foo", "a").await?;
  assert_eq!(result, 1);
  let result: bool = client.hsetnx("foo", "b", 2).await?;
  assert!(result);
  let result: i64 = client.hget("foo", "b").await?;
  assert_eq!(result, 2);

  Ok(())
}

pub async fn should_get_random_field(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hmset("foo", vec![("a", 1), ("b", 2), ("c", 3)]).await?;

  let field: String = client.hrandfield("foo", None).await?;
  assert_contains(vec!["a", "b", "c"], &field.as_str());

  let fields = client.hrandfield("foo", Some((2, false))).await?;
  assert_diff_len(vec!["a", "b", "c"], fields, 1);

  let actual: HashMap<String, i64> = client.hrandfield("foo", Some((2, true))).await?;
  assert_eq!(actual.len(), 2);

  let mut expected: HashMap<String, i64> = HashMap::new();
  expected.insert("a".into(), 1);
  expected.insert("b".into(), 2);
  expected.insert("c".into(), 3);

  for (key, value) in actual.into_iter() {
    let expected_val: i64 = *expected.get(&key).unwrap();
    assert_eq!(value, expected_val);
  }

  Ok(())
}

pub async fn should_get_strlen(client: Client, _: Config) -> Result<(), Error> {
  let expected = "abcdefhijklmnopqrstuvwxyz";
  let _: () = client.hset("foo", ("a", expected)).await?;

  let len: usize = client.hstrlen("foo", "a").await?;
  assert_eq!(len, expected.len());

  Ok(())
}

pub async fn should_get_values(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.hmset("foo", vec![("a", "1"), ("b", "2")]).await?;

  let values: Value = client.hvals("foo").await?;
  assert_diff_len(vec!["1", "2"], values, 0);

  Ok(())
}

#[cfg(feature = "i-hexpire")]
pub async fn should_do_hash_expirations(client: Client, _: Config) -> Result<(), Error> {
  if utils::check_valkey(&client).await {
    return Ok(());
  }

  let _: () = client.hset("foo", [("a", "b"), ("c", "d")]).await?;
  assert_eq!(client.httl::<i64, _, _>("foo", "a").await?, -1);
  assert_eq!(client.hexpire_time::<i64, _, _>("foo", "a").await?, -1);

  let result: i64 = client.hexpire("foo", 60, None, "a").await?;
  assert_eq!(result, 1);
  let result: i64 = client.httl("foo", "a").await?;
  assert!(result > 0);
  let result: i64 = client.hexpire_time("foo", "a").await?;
  assert!(result > 0);

  let result: i64 = client.hpersist("foo", "a").await?;
  assert_eq!(result, 1);
  assert_eq!(client.httl::<i64, _, _>("foo", "a").await?, -1);

  let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 60;
  let result: i64 = client.hexpire_at("foo", time as i64, None, "a").await?;
  assert_eq!(result, 1);
  let result: i64 = client.httl("foo", "a").await?;
  assert!(result > 0);
  let result: i64 = client.hexpire_time("foo", "a").await?;
  assert!(result > 0);

  Ok(())
}

#[cfg(feature = "i-hexpire")]
pub async fn should_do_hash_pexpirations(client: Client, _: Config) -> Result<(), Error> {
  if utils::check_valkey(&client).await {
    return Ok(());
  }

  let _: () = client.hset("foo", [("a", "b"), ("c", "d")]).await?;
  assert_eq!(client.hpttl::<i64, _, _>("foo", "a").await?, -1);
  assert_eq!(client.hpexpire_time::<i64, _, _>("foo", "a").await?, -1);

  let result: i64 = client.hpexpire("foo", 60_000, None, "a").await?;
  assert_eq!(result, 1);
  let result: i64 = client.hpttl("foo", "a").await?;
  assert!(result > 0);
  let result: i64 = client.hpexpire_time("foo", "a").await?;
  assert!(result > 0);

  let result: i64 = client.hpersist("foo", "a").await?;
  assert_eq!(result, 1);
  assert_eq!(client.hpttl::<i64, _, _>("foo", "a").await?, -1);

  let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() + 60_000;
  let result: i64 = client.hpexpire_at("foo", time as i64, None, "a").await?;
  assert_eq!(result, 1);
  let result: i64 = client.hpttl("foo", "a").await?;
  assert!(result > 0);
  let result: i64 = client.hpexpire_time("foo", "a").await?;
  assert!(result > 0);

  Ok(())
}
