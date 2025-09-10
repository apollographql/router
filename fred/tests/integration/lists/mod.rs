use fred::{
  interfaces::*,
  prelude::*,
  types::{
    lists::{LMoveDirection, ListLocation},
    SortOrder,
  },
};
use std::time::Duration;
use tokio::time::sleep;

const COUNT: i64 = 10;

async fn create_count_data(client: &Client, key: &str) -> Result<Vec<Value>, Error> {
  let mut values = Vec::with_capacity(COUNT as usize);
  for idx in 0 .. COUNT {
    let _: () = client.rpush(key, idx).await?;
    values.push(idx.to_string().into());
  }

  Ok(values)
}

pub async fn should_blpop_values(client: Client, _: Config) -> Result<(), Error> {
  let publisher = client.clone_new();
  publisher.connect();
  publisher.wait_for_connect().await?;

  let jh = tokio::spawn(async move {
    for idx in 0 .. COUNT {
      let mut result: Vec<Value> = client.blpop("foo", 30.0).await?;
      assert_eq!(result.pop().unwrap().as_i64().unwrap(), idx);
    }

    Ok::<_, Error>(())
  });

  for idx in 0 .. COUNT {
    // the assertion below checks the length of the list, so we have to make sure not to push faster than elements are
    // removed
    sleep(Duration::from_millis(100)).await;
    let result: i64 = publisher.rpush("foo", idx).await?;
    assert_eq!(result, 1);
  }

  let _ = jh.await?;
  Ok(())
}

pub async fn should_brpop_values(client: Client, _: Config) -> Result<(), Error> {
  let publisher = client.clone_new();
  publisher.connect();
  publisher.wait_for_connect().await?;

  let jh = tokio::spawn(async move {
    for idx in 0 .. COUNT {
      let mut result: Vec<Value> = client.brpop("foo", 30.0).await?;
      assert_eq!(result.pop().unwrap().as_i64().unwrap(), idx);
    }

    Ok::<_, Error>(())
  });

  for idx in 0 .. COUNT {
    // the assertion below checks the length of the list, so we have to make sure not to push faster than elements are
    // removed
    sleep(Duration::from_millis(50)).await;
    let result: i64 = publisher.lpush("foo", idx).await?;
    assert_eq!(result, 1);
  }

  let _ = jh.await?;
  Ok(())
}

pub async fn should_brpoplpush_values(client: Client, _: Config) -> Result<(), Error> {
  let publisher = client.clone_new();
  publisher.connect();
  publisher.wait_for_connect().await?;

  let jh = tokio::spawn(async move {
    for idx in 0 .. COUNT {
      let result: i64 = client.brpoplpush("foo{1}", "bar{1}", 30.0).await?;
      assert_eq!(result, idx);
    }

    Ok::<_, Error>(())
  });

  for idx in 0 .. COUNT {
    let result: i64 = publisher.lpush("foo{1}", idx).await?;
    assert!(result > 0);
  }
  let _ = jh.await?;

  for idx in 0 .. COUNT {
    let result: i64 = publisher.rpop("bar{1}", None).await?;
    assert_eq!(result, idx);
  }

  Ok(())
}

pub async fn should_blmove_values(client: Client, _: Config) -> Result<(), Error> {
  let publisher = client.clone_new();
  publisher.connect();
  publisher.wait_for_connect().await?;

  let jh = tokio::spawn(async move {
    for idx in 0 .. COUNT {
      let result: i64 = client
        .blmove("foo{1}", "bar{1}", LMoveDirection::Right, LMoveDirection::Left, 30.0)
        .await?;
      assert_eq!(result, idx);
    }

    Ok::<_, Error>(())
  });

  for idx in 0 .. COUNT {
    let result: i64 = publisher.lpush("foo{1}", idx).await?;
    assert!(result > 0);
  }
  let _ = jh.await?;

  for idx in 0 .. COUNT {
    let result: i64 = publisher.rpop("bar{1}", None).await?;
    assert_eq!(result, idx);
  }

  Ok(())
}

pub async fn should_lindex_values(client: Client, _: Config) -> Result<(), Error> {
  let expected = create_count_data(&client, "foo").await?;

  for (idx, expected_value) in expected.into_iter().enumerate() {
    let result: Value = client.lindex("foo", idx as i64).await?;
    assert_eq!(result, expected_value);
  }

  Ok(())
}

pub async fn should_linsert_values(client: Client, _: Config) -> Result<(), Error> {
  let result: usize = client.linsert("foo", ListLocation::Before, 1, 0).await?;
  assert_eq!(result, 0);
  let result: usize = client.llen("foo").await?;
  assert_eq!(result, 0);

  let _: () = client.lpush("foo", 0).await?;
  let mut expected: Vec<Value> = vec!["0".into()];
  for idx in 1 .. COUNT {
    let result: i64 = client.linsert("foo", ListLocation::After, idx - 1, idx).await?;
    assert_eq!(result, idx + 1);
    expected.push(idx.to_string().into());
  }
  let values: Vec<Value> = client.lrange("foo", 0, COUNT).await?;
  assert_eq!(values, expected);

  Ok(())
}

pub async fn should_lpop_values(client: Client, _: Config) -> Result<(), Error> {
  let expected = create_count_data(&client, "foo").await?;

  for idx in 0 .. COUNT {
    let result: i64 = client.lpop("foo", None).await?;
    assert_eq!(result, idx);
  }

  let _ = create_count_data(&client, "foo").await?;
  let result: Vec<Value> = client.lpop("foo", Some(COUNT as usize)).await?;
  assert_eq!(result, expected);

  Ok(())
}

pub async fn should_lpos_values(client: Client, _: Config) -> Result<(), Error> {
  let _ = create_count_data(&client, "foo").await?;

  for idx in 0 .. COUNT {
    let result: i64 = client.lpos("foo", idx, None, None, None).await?;
    assert_eq!(result, idx);
  }

  let _ = create_count_data(&client, "foo").await?;
  let _ = create_count_data(&client, "foo").await?;

  for idx in 0 .. COUNT {
    let result: i64 = client.lpos("foo", idx, Some(2), None, None).await?;
    assert_eq!(result, idx + COUNT);
    let result: i64 = client.lpos("foo", idx, Some(3), None, None).await?;
    assert_eq!(result, idx + COUNT * 2);

    let result: Vec<i64> = client.lpos("foo", idx, None, Some(2), None).await?;
    let expected = vec![idx, (idx + COUNT)];
    assert_eq!(result, expected);

    let result: Vec<i64> = client.lpos("foo", idx, None, Some(3), None).await?;
    let expected = vec![idx, (idx + COUNT), (idx + COUNT * 2)];
    assert_eq!(result, expected);
  }

  Ok(())
}

pub async fn should_lpush_values(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. COUNT {
    let result: i64 = client.lpush("foo", idx).await?;
    assert_eq!(result, idx + 1);
    let result: i64 = client.lrange("foo", 0, 0).await?;
    assert_eq!(result, idx);
  }
  let result: i64 = client.llen("foo").await?;
  assert_eq!(result, COUNT);

  Ok(())
}

pub async fn should_lpushx_values(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.lpushx("foo", 0).await?;
  assert_eq!(result, 0);

  let _: () = client.lpush("foo", 0).await?;
  for idx in 0 .. COUNT {
    let result: i64 = client.lpushx("foo", idx).await?;
    assert_eq!(result, idx + 2);
    let result: i64 = client.lrange("foo", 0, 0).await?;
    assert_eq!(result, idx);
  }
  let result: i64 = client.llen("foo").await?;
  assert_eq!(result, COUNT + 1);

  Ok(())
}

pub async fn should_lrange_values(client: Client, _: Config) -> Result<(), Error> {
  let expected = create_count_data(&client, "foo").await?;

  let result: Vec<Value> = client.lrange("foo", 0, COUNT).await?;
  assert_eq!(result, expected);

  for idx in 0 .. COUNT {
    let result: i64 = client.lrange("foo", idx, idx).await?;
    assert_eq!(result, idx);
  }

  Ok(())
}

pub async fn should_lrem_values(client: Client, _: Config) -> Result<(), Error> {
  let _ = create_count_data(&client, "foo").await?;
  for idx in 0 .. COUNT {
    let result: usize = client.lrem("foo", 1, idx).await?;
    assert_eq!(result, 1);
  }
  let result: usize = client.llen("foo").await?;
  assert_eq!(result, 0);

  let _ = create_count_data(&client, "foo").await?;
  let _ = create_count_data(&client, "foo").await?;
  for idx in 0 .. COUNT {
    let result: usize = client.lrem("foo", 2, idx).await?;
    assert_eq!(result, 2);
  }
  let result: usize = client.llen("foo").await?;
  assert_eq!(result, 0);

  Ok(())
}

pub async fn should_lset_values(client: Client, _: Config) -> Result<(), Error> {
  assert!(client.lset::<i64, _, _>("foo", 1, 0).await.is_err());
  let mut expected = create_count_data(&client, "foo").await?;
  expected.reverse();

  for idx in 0 .. COUNT {
    let _: () = client.lset("foo", idx, COUNT - (idx + 1)).await?;
  }
  let result: Vec<Value> = client.lrange("foo", 0, COUNT).await?;
  assert_eq!(result, expected);

  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_ltrim_values(client: Client, _: Config) -> Result<(), Error> {
  let expected = create_count_data(&client, "foo").await?;

  let _: () = client.ltrim("foo", 0, COUNT).await?;
  let result: Vec<Value> = client.lrange("foo", 0, COUNT).await?;
  assert_eq!(result, expected);

  for idx in 0 .. COUNT {
    let _: () = client.ltrim("foo", 0, idx).await?;
    let result: Vec<Value> = client.lrange("foo", 0, COUNT).await?;
    assert_eq!(result, expected[0 .. (idx + 1) as usize]);

    let _: () = client.del("foo").await?;
    let _ = create_count_data(&client, "foo").await?;
  }

  Ok(())
}

pub async fn should_rpop_values(client: Client, _: Config) -> Result<(), Error> {
  let mut expected = create_count_data(&client, "foo").await?;
  expected.reverse();

  for idx in 0 .. COUNT {
    let result: i64 = client.rpop("foo", None).await?;
    assert_eq!(result, COUNT - (idx + 1));
  }

  let _ = create_count_data(&client, "foo").await?;
  let result: Vec<Value> = client.rpop("foo", Some(COUNT as usize)).await?;
  assert_eq!(result, expected);

  Ok(())
}

pub async fn should_rpoplpush_values(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. COUNT {
    let result: i64 = client.lpush("foo{1}", idx).await?;
    assert_eq!(result, 1);
    let result: i64 = client.rpoplpush("foo{1}", "bar{1}").await?;
    assert_eq!(result, idx);
    let result: i64 = client.rpop("bar{1}", None).await?;
    assert_eq!(result, idx);
  }

  Ok(())
}

pub async fn should_lmove_values(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. COUNT {
    let result: i64 = client.lpush("foo{1}", idx).await?;
    assert_eq!(result, 1);
    let result: i64 = client
      .lmove("foo{1}", "bar{1}", LMoveDirection::Right, LMoveDirection::Left)
      .await?;
    assert_eq!(result, idx);
    let result: i64 = client.rpop("bar{1}", None).await?;
    assert_eq!(result, idx);
  }

  Ok(())
}

pub async fn should_rpush_values(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. COUNT {
    let result: i64 = client.rpush("foo", idx).await?;
    assert_eq!(result, idx + 1);
    let result: i64 = client.lrange("foo", -1, -1).await?;
    assert_eq!(result, idx);
  }
  let result: i64 = client.llen("foo").await?;
  assert_eq!(result, COUNT);

  Ok(())
}

pub async fn should_rpushx_values(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.rpushx("foo", 0).await?;
  assert_eq!(result, 0);

  let _: () = client.rpush("foo", 0).await?;
  for idx in 0 .. COUNT {
    let result: i64 = client.rpushx("foo", idx).await?;
    assert_eq!(result, idx + 2);
    let result: i64 = client.lrange("foo", -1, -1).await?;
    assert_eq!(result, idx);
  }
  let result: i64 = client.llen("foo").await?;
  assert_eq!(result, COUNT + 1);

  Ok(())
}

pub async fn should_sort_int_list(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.lpush("foo", vec![1, 2, 3, 4, 5]).await?;

  let sorted: Vec<i64> = client.sort("foo", None, None, (), None, false, None).await?;
  assert_eq!(sorted, vec![1, 2, 3, 4, 5]);
  Ok(())
}

pub async fn should_sort_alpha_list(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.lpush("foo", vec!["a", "b", "c", "d", "e"]).await?;

  let sorted: Vec<String> = client
    .sort("foo", None, None, (), Some(SortOrder::Desc), true, None)
    .await?;
  assert_eq!(sorted, vec!["e", "d", "c", "b", "a"]);
  Ok(())
}

pub async fn should_sort_int_list_with_limit(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.lpush("foo", vec![1, 2, 3, 4, 5]).await?;

  let sorted: Vec<i64> = client.sort("foo", None, Some((2, 2)), (), None, false, None).await?;
  assert_eq!(sorted, vec![3, 4]);
  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_sort_int_list_with_patterns(client: Client, _: Config) -> Result<(), Error> {
  let vals: Vec<i64> = (1 .. 6).collect();
  let key: Key = "foo".into();

  let _: () = client.lpush(&key, vals.clone()).await?;
  for val in vals.iter() {
    // reverse the weights
    let _: () = client
      .set(
        format!("{}_weight_{}", key.as_str().unwrap(), val),
        7 - *val,
        None,
        None,
        false,
      )
      .await?;
  }
  for val in vals.iter() {
    let _: () = client
      .set(
        format!("{}_val_{}", key.as_str().unwrap(), val),
        *val * 2,
        None,
        None,
        false,
      )
      .await?;
  }

  let sorted: Vec<i64> = client
    .sort(
      &key,
      Some(format!("{}_weight_*", key.as_str().unwrap()).into()),
      None,
      format!("{}_val_*", key.as_str().unwrap()),
      None,
      false,
      None,
    )
    .await?;
  assert_eq!(sorted, vec![10, 8, 6, 4, 2]);

  Ok(())
}

#[cfg(feature = "replicas")]
pub async fn should_sort_ro_int_list(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.lpush("foo", vec![1, 2, 3, 4, 5]).await?;
  // wait for replicas to recv the command
  tokio::time::sleep(Duration::from_millis(500)).await;

  let sorted: Vec<i64> = client
    .replicas()
    .sort_ro("foo", None, None, (), Some(SortOrder::Desc), false)
    .await?;
  assert_eq!(sorted, vec![5, 4, 3, 2, 1]);
  Ok(())
}
