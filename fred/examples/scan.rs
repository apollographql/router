#![allow(clippy::disallowed_names)]
#![allow(clippy::let_underscore_future)]
#![allow(dead_code)]

use bytes_utils::Str;
use fred::{prelude::*, types::scan::Scanner};
use futures::stream::TryStreamExt;

async fn create_fake_data(client: &Client) -> Result<(), Error> {
  let values: Vec<(String, i64)> = (0 .. 50).map(|i| (format!("foo-{}", i), i)).collect();
  client.mset(values).await
}

async fn delete_fake_data(client: &Client) -> Result<(), Error> {
  let keys: Vec<_> = (0 .. 50).map(|i| format!("foo-{}", i)).collect();
  client.del::<(), _>(keys).await?;
  Ok(())
}

/// Scan the server, throttling the pagination process so the client only holds one page of keys in memory at a time.
async fn scan_throttled(client: &Client) -> Result<(), Error> {
  // scan all keys in the keyspace, returning 10 keys per page
  let mut scan_stream = client.scan("foo*", Some(10), None);
  while let Some(mut page) = scan_stream.try_next().await? {
    if let Some(keys) = page.take_results() {
      for key in keys.into_iter() {
        let value: Value = client.get(&key).await?;
        println!("Scanned {} -> {:?}", key.as_str_lossy(), value);
      }
    }

    // callers can call `page.next()` to control when the next page is fetched from the server. if this is not called
    // then the next page will be fetched when `page` is dropped.
    page.next();
  }
  Ok(())
}

/// Scan the server as quickly as possible, buffering pending keys in memory on the client.
async fn scan_buffered(client: &Client) -> Result<(), Error> {
  client
    .scan_buffered("foo*", Some(10), None)
    .try_for_each_concurrent(10, |key| async move {
      let value: Value = client.get(&key).await?;
      println!("Scanned {} -> {:?}", key.as_str_lossy(), value);
      Ok(())
    })
    .await
}

/// Example showing how to scan a server one page a time with a custom cursor.
async fn scan_with_cursor(client: &Client) -> Result<(), Error> {
  let mut cursor: Str = "0".into();
  // break out after 1000 records
  let max_keys = 1000;
  let mut count = 0;

  loop {
    let (new_cursor, keys): (Str, Vec<Key>) = client.scan_page(cursor, "*", Some(100), None).await?;
    count += keys.len();

    for key in keys.into_iter() {
      let val: Value = client.get(&key).await?;
      println!("Scanned {} -> {:?}", key.as_str_lossy(), val);
    }

    if count >= max_keys || new_cursor == "0" {
      break;
    } else {
      cursor = new_cursor;
    }
  }
  Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Client::default();
  client.init().await?;
  create_fake_data(&client).await?;

  scan_buffered(&client).await?;
  scan_throttled(&client).await?;
  scan_with_cursor(&client).await?;

  delete_fake_data(&client).await?;
  client.quit().await?;
  Ok(())
}

/// Example showing how to print the memory usage of all keys in a cluster with a `RedisPool`.
///
/// When using a clustered deployment the keyspace will be spread across multiple nodes. However, the cursor in each
/// SCAN command is used to iterate over keys within a single node. There are several ways to concurrently scan
/// all keys on all nodes:
///
/// 1. Use `scan_cluster`.
/// 2. Use `split_cluster` and `scan`.
/// 3. Use `with_cluster_node` and `scan`.
///
/// The best option depends on several factors, but `scan_cluster` is often the easiest approach for most use
/// cases.
async fn pool_scan_cluster_memory_example(pool: &Pool) -> Result<(), Error> {
  // The majority of the client traffic in this scenario comes from the MEMORY USAGE call on each key, so we'll use a
  // pool to round-robin these commands among multiple clients. A single client can scan all nodes in the cluster
  // concurrently, so we use a single client rather than a pool to issue the SCAN calls.
  let mut total_size = 0;
  // if the pattern contains a hash tag then callers can use `scan` instead of `scan_cluster`
  let mut scanner = pool.next().scan_cluster("*", Some(100), None);

  while let Some(mut page) = scanner.try_next().await? {
    if let Some(page) = page.take_results() {
      // pipeline the `MEMORY USAGE` calls
      let pipeline = pool.next().pipeline();
      for key in page.iter() {
        pipeline.memory_usage::<(), _>(key, Some(0)).await?;
      }
      let sizes: Vec<Option<u64>> = pipeline.all().await?;
      assert_eq!(page.len(), sizes.len());

      for (idx, key) in page.into_iter().enumerate() {
        let size = sizes[idx].unwrap_or(0);
        println!("{}: {}", key.as_str_lossy(), size);
        total_size += size;
      }
    }

    page.next();
  }

  println!("Total size: {}", total_size);
  Ok(())
}
