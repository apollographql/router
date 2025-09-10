use fred::{
  cmd,
  prelude::*,
  types::streams::{XCapKind, XCapTrim, XReadResponse, XReadValue, XID},
};
use std::{collections::HashMap, hash::Hash, time::Duration};
use tokio::time::sleep;

type FakeExpectedValues = Vec<HashMap<String, HashMap<String, usize>>>;

async fn create_fake_group_and_stream(client: &Client, key: &str) -> Result<(), Error> {
  client.xgroup_create(key, "group1", "$", true).await
}

async fn add_stream_entries(
  client: &Client,
  key: &str,
  count: usize,
) -> Result<(Vec<String>, FakeExpectedValues), Error> {
  let mut ids = Vec::with_capacity(count);
  let mut expected = Vec::with_capacity(count);
  for idx in 0 .. count {
    let id: String = client.xadd(key, false, None, "*", ("count", idx)).await?;
    ids.push(id.clone());

    let mut outer = HashMap::with_capacity(1);
    let mut inner = HashMap::with_capacity(1);
    inner.insert("count".into(), idx);
    outer.insert(id, inner);
    expected.push(outer);
  }

  Ok((ids, expected))
}

fn has_expected_value(expected: &FakeExpectedValues, actual: &FakeExpectedValues) -> bool {
  actual.iter().enumerate().fold(true, |b, (i, v)| b && v == &expected[i])
}

pub async fn should_xinfo_consumers(client: Client, _: Config) -> Result<(), Error> {
  let result: Result<(), Error> = client.xinfo_consumers("foo{1}", "group1").await;
  assert!(result.is_err());

  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  let consumers: Vec<HashMap<String, String>> = client.xinfo_consumers("foo{1}", "group1").await?;
  assert_eq!(consumers.len(), 1);
  assert_eq!(consumers[0].get("name"), Some(&"consumer1".to_owned()));

  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer2").await?;
  let consumers: Vec<HashMap<String, String>> = client.xinfo_consumers("foo{1}", "group1").await?;
  assert_eq!(consumers.len(), 2);
  assert_eq!(consumers[0].get("name"), Some(&"consumer1".to_owned()));
  assert_eq!(consumers[1].get("name"), Some(&"consumer2".to_owned()));

  Ok(())
}

pub async fn should_xinfo_groups(client: Client, _: Config) -> Result<(), Error> {
  let result: Result<(), Error> = client.xinfo_groups("foo{1}").await;
  assert!(result.is_err());

  create_fake_group_and_stream(&client, "foo{1}").await?;
  let result: Vec<HashMap<String, Value>> = client.xinfo_groups("foo{1}").await?;
  assert_eq!(result.len(), 1);
  assert_eq!(result[0].get("name"), Some(&"group1".into()));

  let _: () = client.xgroup_create("foo{1}", "group2", "$", true).await?;
  let result: Vec<HashMap<String, Value>> = client.xinfo_groups("foo{1}").await?;
  assert_eq!(result.len(), 2);
  assert_eq!(result[0].get("name"), Some(&"group1".into()));
  assert_eq!(result[1].get("name"), Some(&"group2".into()));

  Ok(())
}

pub async fn should_xinfo_streams(client: Client, _: Config) -> Result<(), Error> {
  let result: Result<(), Error> = client.xinfo_stream("foo{1}", true, None).await;
  assert!(result.is_err());

  create_fake_group_and_stream(&client, "foo{1}").await?;
  let mut result: HashMap<String, Value> = client.xinfo_stream("foo{1}", true, None).await?;
  assert!(result.len() >= 6);
  assert_eq!(result.get("length"), Some(&Value::Integer(0)));

  let groups: [HashMap<String, Value>; 1] = result.remove("groups").unwrap().convert()?;
  assert_eq!(groups[0].get("name"), Some(&Value::from("group1")));

  Ok(())
}

pub async fn should_xadd_auto_id_to_a_stream(client: Client, _: Config) -> Result<(), Error> {
  let result: String = client.xadd("foo{1}", false, None, "*", ("a", "b")).await?;
  assert!(!result.is_empty());

  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);
  Ok(())
}

pub async fn should_xadd_manual_id_to_a_stream(client: Client, _: Config) -> Result<(), Error> {
  let result: String = client.xadd("foo{1}", false, None, "1-0", ("a", "b")).await?;
  assert_eq!(result, "1-0");

  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);
  Ok(())
}

pub async fn should_xadd_with_cap_to_a_stream(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client
    .xadd("foo{1}", false, ("MAXLEN", "=", 1), "*", ("a", "b"))
    .await?;

  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);
  Ok(())
}

pub async fn should_xadd_nomkstream_to_a_stream(client: Client, _: Config) -> Result<(), Error> {
  let result: Option<String> = client.xadd("foo{1}", true, None, "*", ("a", "b")).await?;
  assert!(result.is_none());

  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _: () = client.xadd("foo{1}", true, None, "*", ("a", "b")).await?;
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);
  Ok(())
}

pub async fn should_xtrim_a_stream_approx_cap(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;

  let deleted: usize = client.xtrim("foo{1}", ("MAXLEN", "~", 1)).await?;
  assert!(deleted < 3);
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 3 - deleted);

  let _: () = client.custom(cmd!("DEL"), vec!["foo{1}"]).await?;
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let deleted: usize = client
    .xtrim("foo{1}", (XCapKind::MaxLen, XCapTrim::AlmostExact, 1))
    .await?;
  assert!(deleted < 3);
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 3 - deleted);

  Ok(())
}

pub async fn should_xtrim_a_stream_eq_cap(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;

  let deleted: usize = client.xtrim("foo{1}", ("MAXLEN", "=", 1)).await?;
  assert_eq!(deleted, 2);
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);

  let _: () = client.custom(cmd!("DEL"), vec!["foo{1}"]).await?;
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let deleted: usize = client.xtrim("foo{1}", (XCapKind::MaxLen, XCapTrim::Exact, 1)).await?;
  assert_eq!(deleted, 2);
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);

  Ok(())
}

pub async fn should_xdel_one_id_in_a_stream(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (ids, _) = add_stream_entries(&client, "foo{1}", 2).await?;

  let deleted: usize = client.xdel("foo{1}", &ids[0]).await?;
  assert_eq!(deleted, 1);
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);
  Ok(())
}

pub async fn should_xdel_multiple_ids_in_a_stream(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (ids, _) = add_stream_entries(&client, "foo{1}", 3).await?;

  let deleted: usize = client.xdel("foo{1}", ids[0 .. 2].to_vec()).await?;
  assert_eq!(deleted, 2);
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 1);
  Ok(())
}

pub async fn should_xrange_no_count(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (_, expected) = add_stream_entries(&client, "foo{1}", 3).await?;

  let result: FakeExpectedValues = client.xrange("foo{1}", "-", "+", None).await?;
  assert_eq!(result, expected);
  Ok(())
}

pub async fn should_xrange_values_no_count(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (ids, _) = add_stream_entries(&client, "foo{1}", 3).await?;

  let result: Vec<XReadValue<String, String, usize>> = client.xrange_values("foo{1}", "-", "+", None).await?;
  let actual_ids: Vec<String> = result.iter().map(|(id, _)| id.clone()).collect();
  assert_eq!(ids, actual_ids);
  Ok(())
}

pub async fn should_xrevrange_values_no_count(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (mut ids, _) = add_stream_entries(&client, "foo{1}", 3).await?;
  ids.reverse();

  let result: Vec<XReadValue<String, String, usize>> = client.xrevrange_values("foo{1}", "+", "-", None).await?;
  let actual_ids: Vec<String> = result.iter().map(|(id, _)| id.clone()).collect();
  assert_eq!(ids, actual_ids);
  Ok(())
}

pub async fn should_xrange_with_count(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (_, expected) = add_stream_entries(&client, "foo{1}", 3).await?;

  let result: FakeExpectedValues = client.xrange("foo{1}", "-", "+", Some(1)).await?;
  assert!(has_expected_value(&expected, &result));
  Ok(())
}

pub async fn should_xrevrange_no_count(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (_, mut expected) = add_stream_entries(&client, "foo{1}", 3).await?;
  expected.reverse();

  let result: FakeExpectedValues = client.xrevrange("foo{1}", "+", "-", None).await?;
  assert_eq!(result, expected);
  Ok(())
}

pub async fn should_xrevrange_with_count(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (_, mut expected) = add_stream_entries(&client, "foo{1}", 3).await?;
  expected.reverse();

  let result: FakeExpectedValues = client.xrevrange("foo{1}", "-", "+", Some(1)).await?;
  assert!(has_expected_value(&expected, &result));
  Ok(())
}

pub async fn should_run_xlen_on_stream(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 0);

  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 3);
  Ok(())
}

pub async fn should_xread_map_one_key(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;

  let result: XReadResponse<String, String, String, usize> = client.xread_map(None, None, "foo{1}", "0").await?;

  for (idx, (_, record)) in result.get("foo{1}").unwrap().iter().enumerate() {
    let count = record.get("count").expect("Failed to read count");
    assert_eq!(*count, idx);
  }

  Ok(())
}

pub async fn should_xread_one_key_count_1(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let (mut ids, mut expected) = add_stream_entries(&client, "foo{1}", 3).await?;
  let _ = ids.pop().unwrap();
  let most_recent_expected = expected.pop().unwrap();
  let second_recent_id = ids.pop().unwrap();

  let mut expected = HashMap::new();
  expected.insert("foo{1}".into(), vec![most_recent_expected]);

  let result: HashMap<String, Vec<HashMap<String, HashMap<String, usize>>>> = client
    .xread::<Value, _, _>(Some(1), None, "foo{1}", second_recent_id)
    .await?
    .flatten_array_values(1)
    .convert()?;
  assert_eq!(result, expected);

  Ok(())
}

pub async fn should_xread_multiple_keys_count_2(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  create_fake_group_and_stream(&client, "bar{1}").await?;
  let (foo_ids, foo_inner) = add_stream_entries(&client, "foo{1}", 3).await?;
  let (bar_ids, bar_inner) = add_stream_entries(&client, "bar{1}", 3).await?;

  let mut expected = HashMap::new();
  expected.insert("foo{1}".into(), foo_inner[1 ..].to_vec());
  expected.insert("bar{1}".into(), bar_inner[1 ..].to_vec());

  let ids: Vec<XID> = vec![foo_ids[0].as_str().into(), bar_ids[0].as_str().into()];
  let result: HashMap<String, Vec<HashMap<String, HashMap<String, usize>>>> = client
    .xread::<Value, _, _>(Some(2), None, vec!["foo{1}", "bar{1}"], ids)
    .await?
    .flatten_array_values(1)
    .convert()?;
  assert_eq!(result, expected);

  Ok(())
}

pub async fn should_xread_with_blocking(client: Client, _: Config) -> Result<(), Error> {
  let expected_id = "123456789-0";
  create_fake_group_and_stream(&client, "foo{1}").await?;

  let mut expected = HashMap::new();
  let mut inner = HashMap::new();
  let mut fields = HashMap::new();
  fields.insert("count".into(), 100);
  inner.insert(expected_id.into(), fields);
  expected.insert("foo{1}".into(), vec![inner]);

  let add_client = client.clone_new();
  tokio::spawn(async move {
    add_client.connect();
    add_client.wait_for_connect().await?;
    sleep(Duration::from_millis(500)).await;

    let _: () = add_client
      .xadd("foo{1}", false, None, expected_id, ("count", 100))
      .await?;
    add_client.quit().await?;
    Ok::<(), Error>(())
  });

  let result: HashMap<String, Vec<HashMap<String, HashMap<String, usize>>>> = client
    .xread::<Value, _, _>(None, Some(5000), "foo{1}", XID::Max)
    .await?
    .flatten_array_values(1)
    .convert()?;
  assert_eq!(result, expected);

  Ok(())
}

pub async fn should_xgroup_create_no_mkstream(client: Client, _: Config) -> Result<(), Error> {
  let result: Result<Value, Error> = client.xgroup_create("foo{1}", "group1", "$", false).await;
  assert!(result.is_err());
  let _: () = client.xadd("foo{1}", false, None, "*", ("count", 1)).await?;
  let _: () = client.xgroup_create("foo{1}", "group1", "$", false).await?;

  Ok(())
}

pub async fn should_xgroup_create_mkstream(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.xgroup_create("foo{1}", "group1", "$", true).await?;
  let len: usize = client.xlen("foo{1}").await?;
  assert_eq!(len, 0);

  Ok(())
}

pub async fn should_xgroup_createconsumer(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let len: usize = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  assert_eq!(len, 1);

  let consumers: Vec<HashMap<String, Value>> = client.xinfo_consumers("foo{1}", "group1").await?;
  assert_eq!(consumers[0].get("name").unwrap(), &Value::from("consumer1"));
  assert_eq!(consumers[0].get("pending").unwrap(), &Value::from(0));

  Ok(())
}

pub async fn should_xgroup_delconsumer(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let len: usize = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  assert_eq!(len, 1);

  let len: usize = client.xgroup_delconsumer("foo{1}", "group1", "consumer1").await?;
  assert_eq!(len, 0);

  let consumers: Vec<HashMap<String, Value>> = client.xinfo_consumers("foo{1}", "group1").await?;
  assert!(consumers.is_empty());
  Ok(())
}

pub async fn should_xgroup_destroy(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let len: usize = client.xgroup_destroy("foo{1}", "group1").await?;
  assert_eq!(len, 1);

  Ok(())
}

pub async fn should_xgroup_setid(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _: () = client.xgroup_setid("foo{1}", "group1", "12345-0").await?;

  Ok(())
}

pub async fn should_xreadgroup_one_stream(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;

  let result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", None, None, false, "foo{1}", ">")
    .await?;

  assert_eq!(result.len(), 1);
  for (idx, (_, record)) in result.get("foo{1}").unwrap().iter().enumerate() {
    let value = record.get("count").expect("Failed to read count");
    assert_eq!(idx, *value);
  }

  Ok(())
}

pub async fn should_xreadgroup_multiple_stream(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  create_fake_group_and_stream(&client, "bar{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let _ = add_stream_entries(&client, "bar{1}", 1).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  let _: () = client.xgroup_createconsumer("bar{1}", "group1", "consumer1").await?;

  let result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map(
      "group1",
      "consumer1",
      None,
      None,
      false,
      vec!["foo{1}", "bar{1}"],
      vec![">", ">"],
    )
    .await?;

  assert_eq!(result.len(), 2);
  for (idx, (_, record)) in result.get("foo{1}").unwrap().iter().enumerate() {
    let value = record.get("count").expect("Failed to read count");
    assert_eq!(idx, *value);
  }
  let bar_records = result.get("bar{1}").unwrap();
  assert_eq!(bar_records.len(), 1);
  assert_eq!(*bar_records[0].1.get("count").unwrap(), 0);

  Ok(())
}

pub async fn should_xreadgroup_block(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;

  let add_client = client.clone_new();
  tokio::spawn(async move {
    add_client.connect();
    add_client.wait_for_connect().await?;
    sleep(Duration::from_secs(1)).await;

    let _: () = add_client.xadd("foo{1}", false, None, "*", ("count", 100)).await?;
    add_client.quit().await?;
    Ok::<_, Error>(())
  });

  let mut result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", None, Some(10_000), false, "foo{1}", ">")
    .await?;

  assert_eq!(result.len(), 1);
  let records = result.remove("foo{1}").unwrap();
  assert_eq!(records.len(), 1);
  let count = records[0].1.get("count").unwrap();
  assert_eq!(*count, 100);

  Ok(())
}

pub async fn should_xack_one_id(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 1).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;

  let result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", None, None, false, "foo{1}", ">")
    .await?;
  assert_eq!(result.len(), 1);
  let records = result.get("foo{1}").unwrap();
  let id = records[0].0.clone();

  let result: i64 = client.xack("foo{1}", "group1", id).await?;
  assert_eq!(result, 1);
  Ok(())
}

pub async fn should_xack_multiple_ids(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;

  let result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", None, None, false, "foo{1}", ">")
    .await?;
  assert_eq!(result.len(), 1);
  let records = result.get("foo{1}").unwrap();
  let ids: Vec<String> = records.iter().map(|(id, _)| id.clone()).collect();

  let result: i64 = client.xack("foo{1}", "group1", ids).await?;
  assert_eq!(result, 3);
  Ok(())
}

pub async fn should_xclaim_one_id(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer2").await?;

  let mut result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", Some(1), None, false, "foo{1}", ">")
    .await?;
  assert_eq!(result.len(), 1);
  assert_eq!(result.get("foo{1}").unwrap().len(), 1);
  let first_read_id = result.get_mut("foo{1}").unwrap().pop().unwrap().0;
  sleep(Duration::from_secs(1)).await;

  let (total_count, min_id, max_id, consumers): (u64, String, String, Vec<(String, u64)>) =
    client.xpending("foo{1}", "group1", ()).await?;
  assert_eq!(total_count, 1);
  assert_eq!(min_id, first_read_id);
  assert_eq!(max_id, first_read_id);
  assert_eq!(consumers[0], ("consumer1".into(), 1));

  let mut result: Vec<(String, HashMap<String, u64>)> = client
    .xclaim_values(
      "foo{1}",
      "group1",
      "consumer2",
      1000,
      &first_read_id,
      None,
      None,
      None,
      false,
      false,
    )
    .await?;

  assert_eq!(result.len(), 1);
  assert_eq!(result[0].0.as_str(), first_read_id);
  let value = result[0].1.remove("count").unwrap();
  assert_eq!(value, 0);

  let acked: i64 = client.xack("foo{1}", "group1", first_read_id).await?;
  assert_eq!(acked, 1);
  Ok(())
}

pub async fn should_xclaim_multiple_ids(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer2").await?;

  let mut result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", Some(2), None, false, "foo{1}", ">")
    .await?;
  assert_eq!(result.len(), 1);
  assert_eq!(result.get("foo{1}").unwrap().len(), 2);
  let second_read_id = result.get_mut("foo{1}").unwrap().pop().unwrap().0;
  let first_read_id = result.get_mut("foo{1}").unwrap().pop().unwrap().0;
  sleep(Duration::from_secs(1)).await;

  let (total_count, min_id, max_id, consumers): (u64, String, String, Vec<(String, u64)>) =
    client.xpending("foo{1}", "group1", ()).await?;
  assert_eq!(total_count, 2);
  assert_eq!(min_id, first_read_id);
  assert_eq!(max_id, second_read_id);
  assert_eq!(consumers[0], ("consumer1".into(), 2));

  let mut result: Vec<(String, HashMap<String, u64>)> = client
    .xclaim_values(
      "foo{1}",
      "group1",
      "consumer2",
      1000,
      vec![&first_read_id, &second_read_id],
      None,
      None,
      None,
      false,
      false,
    )
    .await?;

  assert_eq!(result.len(), 2);
  assert_eq!(result[0].0.as_str(), first_read_id);
  assert_eq!(result[1].0.as_str(), second_read_id);
  let first_value = result[0].1.remove("count").unwrap();
  let second_value = result[1].1.remove("count").unwrap();
  assert_eq!(first_value, 0);
  assert_eq!(second_value, 1);

  let acked: i64 = client
    .xack("foo{1}", "group1", vec![first_read_id, second_read_id])
    .await?;
  assert_eq!(acked, 2);
  Ok(())
}

pub async fn should_xclaim_with_justid(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer2").await?;

  let mut result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", Some(2), None, false, "foo{1}", ">")
    .await?;
  assert_eq!(result.len(), 1);
  assert_eq!(result.get("foo{1}").unwrap().len(), 2);
  let second_read_id = result.get_mut("foo{1}").unwrap().pop().unwrap().0;
  let first_read_id = result.get_mut("foo{1}").unwrap().pop().unwrap().0;
  sleep(Duration::from_secs(1)).await;

  let (total_count, min_id, max_id, consumers): (u64, String, String, Vec<(String, u64)>) =
    client.xpending("foo{1}", "group1", ()).await?;
  assert_eq!(total_count, 2);
  assert_eq!(min_id, first_read_id);
  assert_eq!(max_id, second_read_id);
  assert_eq!(consumers[0], ("consumer1".into(), 2));

  let result: Vec<String> = client
    .xclaim(
      "foo{1}",
      "group1",
      "consumer2",
      1000,
      vec![&first_read_id, &second_read_id],
      None,
      None,
      None,
      false,
      true,
    )
    .await?;
  assert_eq!(result, vec![first_read_id.clone(), second_read_id.clone()]);

  let acked: i64 = client
    .xack("foo{1}", "group1", vec![first_read_id, second_read_id])
    .await?;
  assert_eq!(acked, 2);
  Ok(())
}

pub async fn should_xautoclaim_default(client: Client, _: Config) -> Result<(), Error> {
  create_fake_group_and_stream(&client, "foo{1}").await?;
  let _ = add_stream_entries(&client, "foo{1}", 3).await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer1").await?;
  let _: () = client.xgroup_createconsumer("foo{1}", "group1", "consumer2").await?;

  let mut result: XReadResponse<String, String, String, usize> = client
    .xreadgroup_map("group1", "consumer1", Some(2), None, false, "foo{1}", ">")
    .await?;
  assert_eq!(result.len(), 1);
  assert_eq!(result.get("foo{1}").unwrap().len(), 2);
  let second_read_id = result.get_mut("foo{1}").unwrap().pop().unwrap().0;
  let first_read_id = result.get_mut("foo{1}").unwrap().pop().unwrap().0;
  sleep(Duration::from_secs(1)).await;

  let (total_count, min_id, max_id, consumers): (u64, String, String, Vec<(String, u64)>) =
    client.xpending("foo{1}", "group1", ()).await?;
  assert_eq!(total_count, 2);
  assert_eq!(min_id, first_read_id);
  assert_eq!(max_id, second_read_id);
  assert_eq!(consumers[0], ("consumer1".into(), 2));

  let (cursor, values): (String, Vec<XReadValue<String, String, usize>>) = client
    .xautoclaim_values("foo{1}", "group1", "consumer2", 1000, "0-0", None, false)
    .await?;

  assert_eq!(cursor, "0-0");
  assert_eq!(values.len(), 2);

  let mut first_expected: HashMap<String, usize> = HashMap::new();
  first_expected.insert("count".into(), 0);
  let mut second_expected: HashMap<String, usize> = HashMap::new();
  second_expected.insert("count".into(), 1);
  assert_eq!(values[0], (first_read_id, first_expected));
  assert_eq!(values[1], (second_read_id, second_expected));

  Ok(())
}
