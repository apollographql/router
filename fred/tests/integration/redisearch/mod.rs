use fred::{
  error::Error,
  prelude::*,
  types::{
    redisearch::{
      AggregateOperation,
      FtAggregateOptions,
      FtCreateOptions,
      FtSearchOptions,
      IndexKind,
      Load,
      SearchSchema,
      SearchSchemaKind,
    },
    Map,
  },
  util::NONE,
};
use maplit::hashmap;
use rand::{thread_rng, Rng};
use redis_protocol::resp3::types::RespVersion;
use std::{collections::HashMap, time::Duration};

pub async fn should_list_indexes(client: Client, _: Config) -> Result<(), Error> {
  assert!(client.ft_list::<Vec<String>>().await?.is_empty());

  let _: () = client
    .ft_create("foo", FtCreateOptions::default(), vec![SearchSchema {
      field_name: "bar".into(),
      alias:      Some("baz".into()),
      kind:       SearchSchemaKind::Numeric {
        sortable: false,
        unf:      false,
        noindex:  false,
      },
    }])
    .await?;

  assert_eq!(client.ft_list::<Vec<String>>().await?, vec!["foo".to_string()]);
  Ok(())
}

pub async fn should_index_and_info_basic_hash(client: Client, _: Config) -> Result<(), Error> {
  assert!(client.ft_list::<Vec<String>>().await?.is_empty());

  let _: () = client
    .ft_create(
      "foo_idx",
      FtCreateOptions {
        on: Some(IndexKind::Hash),
        ..Default::default()
      },
      vec![SearchSchema {
        field_name: "bar".into(),
        alias:      Some("baz".into()),
        kind:       SearchSchemaKind::Text {
          sortable:       false,
          unf:            false,
          noindex:        false,
          phonetic:       None,
          weight:         None,
          withsuffixtrie: false,
          nostem:         false,
        },
      }],
    )
    .await?;

  let _: () = client.hset("foo", ("bar", "abc123")).await?;
  tokio::time::sleep(Duration::from_millis(100)).await;

  let mut info: HashMap<String, Value> = client.ft_info("foo_idx").await?;
  assert_eq!(info.remove("num_docs").unwrap_or(Value::Null).convert::<i64>()?, 1);

  Ok(())
}

pub async fn should_index_and_search_hash(client: Client, _: Config) -> Result<(), Error> {
  assert!(client.ft_list::<Vec<String>>().await?.is_empty());

  let _: () = client
    .ft_create(
      "foo_idx",
      FtCreateOptions {
        on: Some(IndexKind::Hash),
        prefixes: vec!["record:".into()],
        ..Default::default()
      },
      vec![SearchSchema {
        field_name: "bar".into(),
        alias:      None,
        kind:       SearchSchemaKind::Text {
          sortable:       false,
          unf:            false,
          noindex:        false,
          phonetic:       None,
          weight:         None,
          withsuffixtrie: false,
          nostem:         false,
        },
      }],
    )
    .await?;

  let _: () = client.hset("record:1", ("bar", "abc 123")).await?;
  let _: () = client.hset("record:2", ("bar", "abc 345")).await?;
  let _: () = client.hset("record:3", ("bar", "def 678")).await?;
  tokio::time::sleep(Duration::from_millis(100)).await;

  if client.protocol_version() == RespVersion::RESP3 {
    // RESP3 uses maps and includes extra metadata fields
    let mut results: HashMap<String, Value> = client.ft_search("foo_idx", "*", FtSearchOptions::default()).await?;
    assert_eq!(
      results
        .get("total_results")
        .cloned()
        .unwrap_or(Value::Null)
        .convert::<i64>()?,
      3
    );

    // {"attributes":[],"format":"STRING","results":[{"extra_attributes":{"bar":"abc
    // 123"},"id":"record:1","values":[]},{"extra_attributes":{"bar":"abc
    // 345"},"id":"record:2","values":[]},{"extra_attributes":{"bar":"def
    // 678"},"id":"record:3","values":[]}],"total_results":3,"warning":[]}
    let results: Vec<HashMap<String, Value>> = results.remove("results").unwrap().convert()?;
    let expected = vec![
      hashmap! {
        "id" => "record:1".into(),
        "values" => Value::Array(vec![]),
        "extra_attributes" => hashmap! {
          "bar" => "abc 123"
        }.try_into()?
      },
      hashmap! {
        "id" => "record:2".into(),
        "values" => Value::Array(vec![]),
        "extra_attributes" => hashmap! {
          "bar" => "abc 345"
        }.try_into()?
      },
      hashmap! {
        "id" => "record:3".into(),
        "values" => Value::Array(vec![]),
        "extra_attributes" => hashmap! {
          "bar" => "def 678"
        }
        .try_into()?
      },
    ]
    .into_iter()
    .map(|m| {
      m.into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect::<HashMap<String, Value>>()
    })
    .collect::<Vec<_>>();
    assert_eq!(results, expected);
  } else {
    // RESP2 uses an array format w/o extra metadata
    let results: (usize, Key, Key, Key) = client
      .ft_search("foo_idx", "*", FtSearchOptions {
        nocontent: true,
        ..Default::default()
      })
      .await?;
    assert_eq!(results, (3, "record:1".into(), "record:2".into(), "record:3".into()));
    let results: (usize, Key, Key) = client
      .ft_search("foo_idx", "@bar:(abc)", FtSearchOptions {
        nocontent: true,
        ..Default::default()
      })
      .await?;
    assert_eq!(results, (2, "record:1".into(), "record:2".into()));
    let results: (usize, Key, (String, String)) = client
      .ft_search("foo_idx", "@bar:(def)", FtSearchOptions::default())
      .await?;
    assert_eq!(results, (1, "record:3".into(), ("bar".into(), "def 678".into())));
  }

  Ok(())
}

pub async fn should_index_and_aggregate_timestamps(client: Client, _: Config) -> Result<(), Error> {
  assert!(client.ft_list::<Vec<String>>().await?.is_empty());

  // https://redis.io/docs/latest/develop/interact/search-and-query/advanced-concepts/aggregations/
  let _: () = client
    .ft_create(
      "timestamp_idx",
      FtCreateOptions {
        on: Some(IndexKind::Hash),
        prefixes: vec!["record:".into()],
        ..Default::default()
      },
      vec![SearchSchema {
        field_name: "timestamp".into(),
        alias:      None,
        kind:       SearchSchemaKind::Numeric {
          sortable: true,
          unf:      false,
          noindex:  false,
        },
      }],
    )
    .await?;

  for idx in 0 .. 100 {
    let rand: u64 = thread_rng().gen_range(0 .. 10000);
    let _: () = client
      .hset(format!("record:{}", idx), [
        ("timestamp", idx),
        ("user_id", idx + 1000),
        ("rand", rand),
      ])
      .await?;
  }
  tokio::time::sleep(Duration::from_millis(100)).await;

  if client.protocol_version() == RespVersion::RESP3 {
    // RESP3 uses maps and includes extra metadata fields

    // FT.AGGREGATE myIndex "*"
    //   APPLY "@timestamp - (@timestamp % 3600)" AS hour
    let mut result: HashMap<String, Value> = client
      .ft_aggregate("timestamp_idx", "*", FtAggregateOptions {
        load: Some(Load::All),
        pipeline: vec![AggregateOperation::Apply {
          expression: "@timestamp - (@timestamp % 3600)".into(),
          name:       "hour".into(),
        }],
        ..Default::default()
      })
      .await?;

    let results: Vec<Value> = result.remove("results").unwrap().convert()?;
    for (idx, val) in results.into_iter().enumerate() {
      let mut val: HashMap<String, Value> = val.convert()?;
      let mut val: HashMap<String, usize> = val.remove("extra_attributes").unwrap().convert()?;
      assert_eq!(val.remove("timestamp").unwrap(), idx);
      assert_eq!(val.remove("hour").unwrap(), 0);
      assert_eq!(val.remove("user_id").unwrap(), 1000 + idx);
    }
  } else {
    // FT.AGGREGATE myIndex "*"
    //   APPLY "@timestamp - (@timestamp % 3600)" AS hour
    let result: Vec<Value> = client
      .ft_aggregate("timestamp_idx", "*", FtAggregateOptions {
        load: Some(Load::All),
        pipeline: vec![AggregateOperation::Apply {
          expression: "@timestamp - (@timestamp % 3600)".into(),
          name:       "hour".into(),
        }],
        ..Default::default()
      })
      .await?;

    for (idx, val) in result.into_iter().enumerate() {
      if idx == 0 {
        assert_eq!(val.convert::<i64>()?, 1);
      } else {
        let mut val: HashMap<String, usize> = val.convert()?;
        assert_eq!(val.remove("timestamp").unwrap(), idx - 1);
        assert_eq!(val.remove("hour").unwrap(), 0);
        assert_eq!(val.remove("user_id").unwrap(), 1000 + idx - 1);
      }
    }
  }

  // TODO
  // FT.AGGREGATE myIndex "*"
  //   APPLY "@timestamp - (@timestamp % 3600)" AS hour
  //   GROUPBY 1 @hour
  //   	REDUCE COUNT_DISTINCT 1 @user_id AS num_users

  // FT.AGGREGATE myIndex "*"
  //   APPLY "@timestamp - (@timestamp % 3600)" AS hour
  //   GROUPBY 1 @hour
  //   	REDUCE COUNT_DISTINCT 1 @user_id AS num_users
  //   SORTBY 2 @hour ASC

  // FT.AGGREGATE myIndex "*"
  //   APPLY "@timestamp - (@timestamp % 3600)" AS hour
  //   GROUPBY 1 @hour
  //   	REDUCE COUNT_DISTINCT 1 @user_id AS num_users
  //   SORTBY 2 @hour ASC
  //   APPLY timefmt(@hour) AS hour

  Ok(())
}
