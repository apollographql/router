#![allow(dead_code)]
use fred::{
  prelude::*,
  types::scan::{ScanResult, Scanner},
};
use futures::{Stream, TryStreamExt};
// tokio_stream has a more flexible version of `collect`
use bytes_utils::Str;
use tokio_stream::StreamExt;

const SCAN_KEYS: i64 = 100;

#[cfg(feature = "i-keys")]
pub async fn should_scan_keyspace(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. SCAN_KEYS {
    let _: () = client
      .set(format!("foo-{}-{}", idx, "{1}"), idx, None, None, false)
      .await?;
  }

  let count = client
    .scan("foo*{1}", Some(10), None)
    .try_fold(0, |mut count, mut result| async move {
      if let Some(results) = result.take_results() {
        count += results.len() as i64;
        // scanning wont return results in any particular order, so we just check the format of the key

        for key in results.into_iter() {
          let parts: Vec<&str> = key.as_str().unwrap().split('-').collect();
          assert!(parts[1].parse::<i64>().is_ok());
        }
      } else {
        panic!("Empty results in scan.");
      }

      result.next();
      Ok(count)
    })
    .await?;

  assert_eq!(count, SCAN_KEYS);
  Ok(())
}

#[cfg(feature = "i-hashes")]
pub async fn should_hscan_hash(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. SCAN_KEYS {
    let value = (format!("bar-{}", idx), idx);
    let _: () = client.hset("foo", value).await?;
  }

  let count = client
    .hscan("foo", "bar*", Some(10))
    .try_fold(0_i64, |mut count, mut result| async move {
      if let Some(results) = result.take_results() {
        count += results.len() as i64;

        // scanning wont return results in any particular order, so we just check the format of the key
        for (key, _) in results.iter() {
          let parts: Vec<&str> = key.as_str().unwrap().split('-').collect();
          assert!(parts[1].parse::<i64>().is_ok());
        }
      } else {
        panic!("Empty results in hscan.");
      }

      result.next();
      Ok(count)
    })
    .await?;

  assert_eq!(count, SCAN_KEYS);
  Ok(())
}

#[cfg(feature = "i-sets")]
pub async fn should_sscan_set(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. SCAN_KEYS {
    let _: () = client.sadd("foo", idx).await?;
  }

  let count = client
    .sscan("foo", "*", Some(10))
    .try_fold(0_i64, |mut count, mut result| async move {
      if let Some(results) = result.take_results() {
        count += results.len() as i64;

        for value in results.into_iter() {
          assert!(value.as_i64().is_some());
        }
      } else {
        panic!("Empty sscan result");
      }

      result.next();
      Ok(count)
    })
    .await?;

  assert_eq!(count, SCAN_KEYS);
  Ok(())
}

#[cfg(feature = "i-sorted-sets")]
pub async fn should_zscan_sorted_set(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. SCAN_KEYS {
    let (score, value) = (idx as f64, format!("foo-{}", idx));
    let _: () = client.zadd("foo", None, None, false, false, (score, value)).await?;
  }

  let count = client
    .zscan("foo", "*", Some(10))
    .try_fold(0_i64, |mut count, mut result| async move {
      if let Some(results) = result.take_results() {
        count += results.len() as i64;

        for (value, score) in results.into_iter() {
          let value_str = value.as_str().unwrap();
          let parts: Vec<&str> = value_str.split('-').collect();
          let value_suffix = parts[1].parse::<f64>().unwrap();

          assert_eq!(value_suffix, score);
        }
      } else {
        panic!("Empty zscan result");
      }

      result.next();
      Ok(count)
    })
    .await?;

  assert_eq!(count, SCAN_KEYS);
  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_scan_cluster(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. 2000 {
    let _: () = client.set(idx, idx, None, None, false).await?;
  }

  let mut count = 0;
  let mut scan_stream = client.scan_cluster("*", Some(10), None);
  while let Some(Ok(mut page)) = scan_stream.next().await {
    let results = page.take_results();
    count += results.unwrap().len();
    page.next();
  }

  assert_eq!(count, 2000);
  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_scan_buffered(client: Client, _: Config) -> Result<(), Error> {
  let mut expected = Vec::with_capacity(100);
  for idx in 0 .. 100 {
    // write everything to the same cluster node
    let key: Key = format!("foo-{{1}}-{}", idx).into();
    expected.push(key.clone());
    let _: () = client.set(key, idx, None, None, false).await?;
  }
  expected.sort();

  let mut keys: Vec<Key> = client
    .scan_buffered("foo-{1}*", Some(20), None)
    .collect::<Result<Vec<Key>, Error>>()
    .await?;
  keys.sort();

  assert_eq!(keys, expected);
  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_scan_cluster_buffered(client: Client, _: Config) -> Result<(), Error> {
  let mut expected = Vec::with_capacity(100);
  for idx in 0 .. 100 {
    let key: Key = format!("foo-{}", idx).into();
    expected.push(key.clone());
    let _: () = client.set(key, idx, None, None, false).await?;
  }
  expected.sort();

  let mut keys: Vec<Key> = client
    .scan_cluster_buffered("foo*", Some(20), None)
    .collect::<Result<Vec<Key>, Error>>()
    .await?;
  keys.sort();

  assert_eq!(keys, expected);
  Ok(())
}

#[cfg(feature = "i-keys")]
fn scan_all(client: &Client, page_size: Option<u32>) -> impl Stream<Item = Result<ScanResult, Error>> {
  use futures::StreamExt;

  if client.is_clustered() {
    client.scan_cluster("*", page_size, None).boxed()
  } else {
    client.scan("*", page_size, None).boxed()
  }
}

#[cfg(feature = "i-keys")]
pub async fn should_continue_scanning_on_page_drop(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. 100 {
    let key: Key = format!("foo-{}", idx).into();
    let _: () = client.set(key, idx, None, None, false).await?;
  }

  let mut count = 0;
  let mut scanner = scan_all(&client, Some(10));
  while let Some(Ok(mut page)) = scanner.next().await {
    let keys = page.take_results().unwrap();
    count += keys.len();
  }
  assert_eq!(count, 100);

  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_scan_by_page_centralized(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. 100 {
    let key: Key = format!("foo-{}", idx).into();
    let _: () = client.set(key, idx, None, None, false).await?;
  }
  let mut cursor: Str = "0".into();
  let mut count = 0;

  loop {
    let (new_cursor, keys): (Str, Vec<Key>) = client.scan_page(cursor, "*", None, None).await?;
    count += keys.len();

    if new_cursor == "0" {
      break;
    } else {
      cursor = new_cursor;
    }
  }

  assert_eq!(count, 100);
  Ok(())
}

#[cfg(all(feature = "i-keys", feature = "i-cluster"))]
pub async fn should_scan_by_page_clustered(client: Client, _: Config) -> Result<(), Error> {
  for idx in 0 .. 100 {
    let key: Key = format!("foo-{{1}}-{idx}").into();
    let _: () = client.set(key, idx, None, None, false).await?;
  }
  let mut cursor: Str = "0".into();
  let mut count = 0;

  let server = client
    .cached_cluster_state()
    .and_then(|state| {
      let slot = redis_protocol::redis_keyslot(b"foo-{1}-0");
      state.get_server(slot).cloned()
    })
    .unwrap();

  loop {
    let (new_cursor, keys): (Str, Vec<Key>) = client
      .with_cluster_node(&server)
      .scan_page(cursor, "*", None, None)
      .await?;
    count += keys.len();

    if new_cursor == "0" {
      break;
    } else {
      cursor = new_cursor;
    }
  }

  assert_eq!(count, 100);
  Ok(())
}
