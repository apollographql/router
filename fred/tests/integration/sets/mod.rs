use fred::prelude::*;
use std::collections::HashSet;

fn vec_to_set(data: Vec<Value>) -> HashSet<Value> {
  let mut out = HashSet::with_capacity(data.len());
  for value in data.into_iter() {
    out.insert(value);
  }
  out
}

// #[cfg(feature = "index-map")]
// fn sets_eq(lhs: &IndexSet<RedisValue>, rhs: &HashSet<RedisValue>) -> bool {
// let lhs: HashSet<RedisValue> = lhs.iter().map(|v| v.clone()).collect();
// &lhs == rhs
// }

fn sets_eq(lhs: &HashSet<Value>, rhs: &HashSet<Value>) -> bool {
  lhs == rhs
}

pub async fn should_sadd_elements(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.sadd("foo", "a").await?;
  assert_eq!(result, 1);
  let result: i64 = client.sadd("foo", vec!["b", "c"]).await?;
  assert_eq!(result, 2);
  let result: i64 = client.sadd("foo", vec!["c", "d"]).await?;
  assert_eq!(result, 1);

  Ok(())
}

pub async fn should_scard_elements(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.scard("foo").await?;
  assert_eq!(result, 0);

  let result: i64 = client.sadd("foo", vec!["1", "2", "3", "4", "5"]).await?;
  assert_eq!(result, 5);
  let result: i64 = client.scard("foo").await?;
  assert_eq!(result, 5);

  Ok(())
}

pub async fn should_sdiff_elements(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo{1}", vec![1, 2, 3, 4, 5, 6]).await?;
  let _: () = client.sadd("bar{1}", vec![3, 4, 5, 6, 7, 8]).await?;
  let result: HashSet<Value> = client.sdiff(vec!["foo{1}", "bar{1}"]).await?;

  assert!(sets_eq(&result, &vec_to_set(vec!["1".into(), "2".into()])));
  Ok(())
}

pub async fn should_sdiffstore_elements(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo{1}", vec![1, 2, 3, 4, 5, 6]).await?;
  let _: () = client.sadd("bar{1}", vec![3, 4, 5, 6, 7, 8]).await?;
  let result: i64 = client.sdiffstore("baz{1}", vec!["foo{1}", "bar{1}"]).await?;
  assert_eq!(result, 2);
  let result: HashSet<Value> = client.smembers("baz{1}").await?;

  assert!(sets_eq(&result, &vec_to_set(vec!["1".into(), "2".into()])));
  Ok(())
}

pub async fn should_sinter_elements(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo{1}", vec![1, 2, 3, 4, 5, 6]).await?;
  let _: () = client.sadd("bar{1}", vec![3, 4, 5, 6, 7, 8]).await?;
  let result: HashSet<Value> = client.sinter(vec!["foo{1}", "bar{1}"]).await?;

  assert!(sets_eq(
    &result,
    &vec_to_set(vec!["3".into(), "4".into(), "5".into(), "6".into()])
  ));

  Ok(())
}

pub async fn should_sinterstore_elements(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo{1}", vec![1, 2, 3, 4, 5, 6]).await?;
  let _: () = client.sadd("bar{1}", vec![3, 4, 5, 6, 7, 8]).await?;
  let result: i64 = client.sinterstore("baz{1}", vec!["foo{1}", "bar{1}"]).await?;
  assert_eq!(result, 4);
  let result: HashSet<Value> = client.smembers("baz{1}").await?;

  assert!(sets_eq(
    &result,
    &vec_to_set(vec!["3".into(), "4".into(), "5".into(), "6".into()])
  ));

  Ok(())
}

pub async fn should_check_sismember(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo", vec![1, 2, 3, 4, 5, 6]).await?;

  let result: bool = client.sismember("foo", 1).await?;
  assert!(result);
  let result: bool = client.sismember("foo", 7).await?;
  assert!(!result);

  Ok(())
}

pub async fn should_check_smismember(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo", vec![1, 2, 3, 4, 5, 6]).await?;

  let result: Vec<bool> = client.smismember("foo", vec![1, 2, 7]).await?;
  assert!(result[0]);
  assert!(result[1]);
  assert!(!result[2]);

  let result: bool = client.sismember("foo", 7).await?;
  assert!(!result);

  Ok(())
}

pub async fn should_read_smembers(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo", vec![1, 2, 3, 4, 5, 6]).await?;
  let result: HashSet<Value> = client.smembers("foo").await?;
  assert!(sets_eq(
    &result,
    &vec_to_set(vec![
      "1".into(),
      "2".into(),
      "3".into(),
      "4".into(),
      "5".into(),
      "6".into()
    ])
  ));

  Ok(())
}

pub async fn should_smove_elements(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo{1}", vec![1, 2, 3, 4, 5, 6]).await?;
  let _: () = client.sadd("bar{1}", 5).await?;

  let result: i64 = client.smove("foo{1}", "bar{1}", 7).await?;
  assert_eq!(result, 0);
  let result: i64 = client.smove("foo{1}", "bar{1}", 5).await?;
  assert_eq!(result, 1);
  let result: i64 = client.smove("foo{1}", "bar{1}", 1).await?;
  assert_eq!(result, 1);

  let foo: HashSet<Value> = client.smembers("foo{1}").await?;
  let bar: HashSet<Value> = client.smembers("bar{1}").await?;
  assert!(sets_eq(
    &foo,
    &vec_to_set(vec!["2".into(), "3".into(), "4".into(), "6".into()])
  ));
  assert!(sets_eq(&bar, &vec_to_set(vec!["5".into(), "1".into()])));

  Ok(())
}

pub async fn should_spop_elements(client: Client, _: Config) -> Result<(), Error> {
  let expected = vec_to_set(vec!["1".into(), "2".into(), "3".into()]);
  let _: () = client.sadd("foo", vec![1, 2, 3]).await?;

  let result = client.spop("foo", None).await?;
  assert!(expected.contains(&result));

  let result: Vec<Value> = client.spop("foo", Some(3)).await?;
  for value in result.into_iter() {
    assert!(expected.contains(&value));
  }

  Ok(())
}

pub async fn should_get_random_member(client: Client, _: Config) -> Result<(), Error> {
  let expected = vec_to_set(vec!["1".into(), "2".into(), "3".into()]);
  let _: () = client.sadd("foo", vec![1, 2, 3]).await?;

  let result = client.srandmember("foo", None).await?;
  assert!(expected.contains(&result));
  let result: Vec<Value> = client.srandmember("foo", Some(3)).await?;
  for value in result.into_iter() {
    assert!(expected.contains(&value));
  }

  Ok(())
}

pub async fn should_remove_elements(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.srem("foo", 1).await?;
  assert_eq!(result, 0);

  let _: () = client.sadd("foo", vec![1, 2, 3, 4, 5, 6]).await?;
  let result: i64 = client.srem("foo", 1).await?;
  assert_eq!(result, 1);
  let result: i64 = client.srem("foo", vec![2, 3, 4, 7]).await?;
  assert_eq!(result, 3);

  let result: HashSet<Value> = client.smembers("foo").await?;
  assert!(sets_eq(&result, &vec_to_set(vec!["5".into(), "6".into()])));

  Ok(())
}

pub async fn should_sunion_elements(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo{1}", vec![1, 2, 3, 4, 5, 6]).await?;
  let _: () = client.sadd("bar{1}", vec![3, 4, 5, 6, 7, 8]).await?;
  let result: HashSet<Value> = client.sunion(vec!["foo{1}", "bar{1}"]).await?;

  assert!(sets_eq(
    &result,
    &vec_to_set(vec![
      "1".into(),
      "2".into(),
      "3".into(),
      "4".into(),
      "5".into(),
      "6".into(),
      "7".into(),
      "8".into()
    ])
  ));

  Ok(())
}

pub async fn should_sunionstore_elements(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.sadd("foo{1}", vec![1, 2, 3, 4, 5, 6]).await?;
  let _: () = client.sadd("bar{1}", vec![3, 4, 5, 6, 7, 8]).await?;
  let result: i64 = client.sunionstore("baz{1}", vec!["foo{1}", "bar{1}"]).await?;
  assert_eq!(result, 8);
  let result: HashSet<Value> = client.smembers("baz{1}").await?;

  assert!(sets_eq(
    &result,
    &vec_to_set(vec![
      "1".into(),
      "2".into(),
      "3".into(),
      "4".into(),
      "5".into(),
      "6".into(),
      "7".into(),
      "8".into()
    ])
  ));

  Ok(())
}
