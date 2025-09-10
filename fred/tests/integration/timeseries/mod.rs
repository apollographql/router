use bytes_utils::Str;
use fred::{
  clients::Client,
  error::Error,
  interfaces::*,
  prelude::FredResult,
  types::{
    config::Config,
    timeseries::{Aggregator, GetLabels, Resp2TimeSeriesValues, Resp3TimeSeriesValues, Timestamp},
    Key,
    Value,
  },
};
use redis_protocol::resp3::types::RespVersion;
use std::{collections::HashMap, time::Duration};
use tokio::time::sleep;

pub async fn should_ts_add_get_and_range(client: Client, _: Config) -> Result<(), Error> {
  let first_timestamp: i64 = client.ts_add("foo", "*", 41.0, None, None, None, None, ()).await?;
  assert!(first_timestamp > 0);
  sleep(Duration::from_millis(5)).await;
  let second_timestamp: i64 = client.ts_add("foo", "*", 42.0, None, None, None, None, ()).await?;
  sleep(Duration::from_millis(5)).await;
  assert!(second_timestamp > 0);
  assert!(second_timestamp > first_timestamp);
  let (timestamp, latest): (i64, f64) = client.ts_get("foo", true).await?;
  assert_eq!(latest, 42.0);
  assert_eq!(timestamp, second_timestamp);

  let range: Vec<(i64, f64)> = client.ts_range("foo", "-", "+", true, [], None, None, None).await?;
  assert_eq!(range, vec![(first_timestamp, 41.0), (second_timestamp, 42.0)]);
  Ok(())
}

pub async fn should_create_alter_and_del_timeseries(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.ts_create("foo{1}", None, None, None, None, ("a", "b")).await?;
  let _: () = client.ts_alter("foo{1}", None, None, None, ("b", "c")).await?;

  Ok(())
}

pub async fn should_madd_and_mget(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.ts_create("foo{1}", None, None, None, None, ("a", "b")).await?;
  let _: () = client.ts_create("bar{1}", None, None, None, None, ("a", "b")).await?;

  let values = vec![
    ("foo{1}", 1, 1.1),
    ("foo{1}", 2, 2.2),
    ("foo{1}", 3, 3.3),
    ("bar{1}", 1, 1.2),
    ("bar{1}", 2, 2.3),
  ];

  let args: Vec<_> = values.clone().into_iter().map(|(k, t, v)| (k, t.into(), v)).collect();
  let timestamps: Vec<i64> = client.ts_madd(args).await?;
  assert_eq!(timestamps, vec![1, 2, 3, 1, 2]);

  let mut keys: Vec<String> = client.ts_queryindex(["a=b"]).await?;
  keys.sort();
  assert_eq!(keys, vec!["bar{1}", "foo{1}"]);

  if client.protocol_version() == RespVersion::RESP2 {
    let mut values: Resp2TimeSeriesValues<String, String, String> =
      client.ts_mget(false, Some(GetLabels::WithLabels), ["a=b"]).await?;
    values.sort_by(|(lhs_key, _, _), (rhs_key, _, _)| lhs_key.cmp(rhs_key));

    let expected = vec![
      ("bar{1}".to_string(), vec![("a".to_string(), "b".to_string())], vec![(
        2, 2.3,
      )]),
      ("foo{1}".to_string(), vec![("a".to_string(), "b".to_string())], vec![(
        3, 3.3,
      )]),
    ];
    assert_eq!(values, expected);
  } else {
    let values: Resp3TimeSeriesValues<String, String, String> =
      client.ts_mget(false, Some(GetLabels::WithLabels), ["a=b"]).await?;

    let mut expected = HashMap::new();
    expected.insert(
      "foo{1}".to_string(),
      (vec![("a".to_string(), "b".to_string())], vec![(3, 3.3)]),
    );
    expected.insert(
      "bar{1}".to_string(),
      (vec![("a".to_string(), "b".to_string())], vec![(2, 2.3)]),
    );
    assert_eq!(values, expected);
  }
  Ok(())
}

pub async fn should_incr_and_decr(client: Client, _: Config) -> Result<(), Error> {
  // taken from the docs
  let timestamp: i64 = client
    .ts_incrby(
      "foo",
      232.0,
      Some(Timestamp::Custom(1657811829000)),
      None,
      false,
      None,
      (),
    )
    .await?;
  assert_eq!(timestamp, 1657811829000);
  let timestamp: i64 = client
    .ts_incrby(
      "foo",
      157.0,
      Some(Timestamp::Custom(1657811829000)),
      None,
      false,
      None,
      (),
    )
    .await?;
  assert_eq!(timestamp, 1657811829000);
  let timestamp: i64 = client
    .ts_decrby(
      "foo",
      157.0,
      Some(Timestamp::Custom(1657811829000)),
      None,
      false,
      None,
      (),
    )
    .await?;
  assert_eq!(timestamp, 1657811829000);

  Ok(())
}

pub async fn should_create_and_delete_rules(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client
    .ts_create("temp:TLV", None, None, None, None, [
      ("type", "temp"),
      ("location", "TLV"),
    ])
    .await?;
  let _: () = client
    .ts_create("dailyAvgTemp:TLV", None, None, None, None, [
      ("type", "temp"),
      ("location", "TLV"),
    ])
    .await?;
  let _: () = client
    .ts_createrule("temp:TLV", "dailyAvgTemp:TLV", (Aggregator::TWA, 86400000), None)
    .await?;
  let _: () = client.ts_deleterule("temp:TLV", "dailyAvgTemp:TLV").await?;

  Ok(())
}

pub async fn should_madd_and_mrange(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.ts_create("foo{1}", None, None, None, None, ("a", "b")).await?;
  let _: () = client.ts_create("bar{1}", None, None, None, None, ("a", "b")).await?;

  let values = vec![
    ("foo{1}", 1, 1.1),
    ("foo{1}", 2, 2.2),
    ("foo{1}", 3, 3.3),
    ("bar{1}", 1, 1.2),
    ("bar{1}", 2, 2.3),
  ];
  let args: Vec<_> = values.clone().into_iter().map(|(k, t, v)| (k, t.into(), v)).collect();
  let timestamps: Vec<i64> = client.ts_madd(args).await?;
  assert_eq!(timestamps, vec![1, 2, 3, 1, 2]);

  if client.protocol_version() == RespVersion::RESP2 {
    let mut samples: Resp2TimeSeriesValues<String, String, String> = client
      .ts_mrange(
        "-",
        "+",
        false,
        None,
        None,
        Some(GetLabels::WithLabels),
        None,
        None,
        ["a=b"],
        None,
      )
      .await?;
    samples.sort_by(|(l, _, _), (r, _, _)| l.cmp(r));

    let expected = vec![
      ("bar{1}".to_string(), vec![("a".to_string(), "b".to_string())], vec![
        (1, 1.2),
        (2, 2.3),
      ]),
      ("foo{1}".to_string(), vec![("a".to_string(), "b".to_string())], vec![
        (1, 1.1),
        (2, 2.2),
        (3, 3.3),
      ]),
    ];
    assert_eq!(samples, expected)
  } else {
    // RESP3 has an additional (undocumented?) aggregators section
    // Array([
    // 	String("bar{1}"),
    // 	Array([
    // 		Array([
    // 			String("a"),
    // 			String("b")
    // 		]),
    // 		Array([
    // 			String("aggregators"),
    // 			Array([])
    // 		]),
    // 		Array([
    // 			Array([
    // 				Integer(1),
    // 				Double(1.2)
    // 			]),
    // 			Array([
    // 				Integer(2),
    // 				Double(2.3)
    // 			])
    // 		])
    // 	]),
    // 	String("foo{1}"),
    // 	Array([
    // 		Array([
    // 			String("a"),
    // 			String("b")
    // 		]),
    // 		Array([
    // 			String("aggregators"),
    // 			Array([])
    // 		]),
    // 		Array([
    // 			Array([
    // 				Integer(1),
    // 				Double(1.1)
    // 			]),
    // 			Array([
    // 				Integer(2),
    // 				Double(2.2)
    // 			]),
    // 			Array([
    // 				Integer(3),
    // 				Double(3.3)
    // 			])
    // 		])
    // 	])
    // ])
    //
    // TODO add another TimeSeriesValues type alias for this?

    let samples: HashMap<String, (Vec<(String, String)>, Vec<Value>, Vec<(i64, f64)>)> = client
      .ts_mrange(
        "-",
        "+",
        false,
        None,
        None,
        Some(GetLabels::WithLabels),
        None,
        None,
        ["a=b"],
        None,
      )
      .await?;

    let mut expected = HashMap::new();
    expected.insert(
      "foo{1}".to_string(),
      (
        vec![("a".to_string(), "b".to_string())],
        vec!["aggregators".as_bytes().into(), Value::Array(vec![])],
        vec![(1, 1.1), (2, 2.2), (3, 3.3)],
      ),
    );
    expected.insert(
      "bar{1}".to_string(),
      (
        vec![("a".to_string(), "b".to_string())],
        vec!["aggregators".as_bytes().into(), Value::Array(vec![])],
        vec![(1, 1.2), (2, 2.3)],
      ),
    );
    assert_eq!(samples, expected)
  }

  Ok(())
}

pub async fn should_madd_and_mrevrange(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.ts_create("foo{1}", None, None, None, None, ("a", "b")).await?;
  let _: () = client.ts_create("bar{1}", None, None, None, None, ("a", "b")).await?;

  let values = vec![
    ("foo{1}", 1, 1.1),
    ("foo{1}", 2, 2.2),
    ("foo{1}", 3, 3.3),
    ("bar{1}", 1, 1.2),
    ("bar{1}", 2, 2.3),
  ];
  let args: Vec<_> = values.clone().into_iter().map(|(k, t, v)| (k, t.into(), v)).collect();
  let timestamps: Vec<i64> = client.ts_madd(args).await?;
  assert_eq!(timestamps, vec![1, 2, 3, 1, 2]);

  if client.protocol_version() == RespVersion::RESP2 {
    let mut samples: Resp2TimeSeriesValues<String, String, String> = client
      .ts_mrevrange(
        "-",
        "+",
        false,
        None,
        None,
        Some(GetLabels::WithLabels),
        None,
        None,
        ["a=b"],
        None,
      )
      .await?;
    samples.sort_by(|(l, _, _), (r, _, _)| l.cmp(r));

    let expected = vec![
      ("bar{1}".to_string(), vec![("a".to_string(), "b".to_string())], vec![
        (2, 2.3),
        (1, 1.2),
      ]),
      ("foo{1}".to_string(), vec![("a".to_string(), "b".to_string())], vec![
        (3, 3.3),
        (2, 2.2),
        (1, 1.1),
      ]),
    ];
    assert_eq!(samples, expected)
  } else {
    // see the mrange test above for more info on this section

    let samples: HashMap<String, (Vec<(String, String)>, Vec<Value>, Vec<(i64, f64)>)> = client
      .ts_mrevrange(
        "-",
        "+",
        false,
        None,
        None,
        Some(GetLabels::WithLabels),
        None,
        None,
        ["a=b"],
        None,
      )
      .await?;

    let mut expected = HashMap::new();
    expected.insert(
      "foo{1}".to_string(),
      (
        vec![("a".to_string(), "b".to_string())],
        vec!["aggregators".as_bytes().into(), Value::Array(vec![])],
        vec![(3, 3.3), (2, 2.2), (1, 1.1)],
      ),
    );
    expected.insert(
      "bar{1}".to_string(),
      (
        vec![("a".to_string(), "b".to_string())],
        vec!["aggregators".as_bytes().into(), Value::Array(vec![])],
        vec![(2, 2.3), (1, 1.2)],
      ),
    );
    assert_eq!(samples, expected)
  }

  Ok(())
}
