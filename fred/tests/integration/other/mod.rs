use super::utils;
use async_trait::async_trait;
use fred::{
  clients::{Client, Pool},
  cmd,
  error::{Error, ErrorKind},
  interfaces::*,
  prelude::{Blocking, Server, Value},
  types::{
    config::{ClusterDiscoveryPolicy, Config, Options, PerformanceConfig, ServerConfig},
    Builder,
    ClusterHash,
    Key,
    Map,
  },
};
use futures::future::try_join;
use parking_lot::RwLock;
use redis_protocol::resp3::types::RespVersion;
use std::{
  collections::{BTreeMap, BTreeSet, HashMap},
  convert::TryInto,
  mem,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
  time::Duration,
};
use tokio::time::sleep;

#[cfg(feature = "subscriber-client")]
use fred::clients::SubscriberClient;
#[cfg(feature = "credential-provider")]
use fred::types::config::CredentialProvider;
#[cfg(feature = "replicas")]
use fred::types::config::ReplicaConfig;
#[cfg(feature = "partial-tracing")]
use fred::types::config::TracingConfig;
#[cfg(feature = "i-client")]
use fred::types::ClientUnblockFlag;
#[cfg(feature = "dns")]
use fred::types::Resolve;
#[cfg(feature = "dns")]
use hickory_resolver::{config::*, TokioAsyncResolver};
#[cfg(feature = "dns")]
use std::net::{IpAddr, SocketAddr};
use tokio::task::JoinSet;

#[cfg(all(feature = "i-keys", feature = "i-hashes"))]
fn hash_to_btree(vals: &Map) -> BTreeMap<Key, u16> {
  vals
    .iter()
    .map(|(key, value)| (key.clone(), value.as_u64().unwrap() as u16))
    .collect()
}

#[cfg(all(feature = "i-keys", feature = "i-hashes"))]
fn array_to_set<T: Ord>(vals: Vec<T>) -> BTreeSet<T> {
  vals.into_iter().collect()
}

#[cfg(feature = "i-keys")]
pub fn incr_atomic(size: &Arc<AtomicUsize>) -> usize {
  size.fetch_add(1, Ordering::AcqRel).saturating_add(1)
}

#[cfg(all(feature = "i-keys", feature = "i-hashes"))]
pub async fn should_smoke_test_from_value_impl(client: Client, _: Config) -> Result<(), Error> {
  let nested_values: Map = vec![("a", 1), ("b", 2)].try_into()?;
  let _: () = client.set("foo", "123", None, None, false).await?;
  let _: () = client.set("baz", "456", None, None, false).await?;
  let _: () = client.hset("bar", &nested_values).await?;

  let foo: usize = client.get("foo").await?;
  assert_eq!(foo, 123);
  let foo: i64 = client.get("foo").await?;
  assert_eq!(foo, 123);
  let foo: String = client.get("foo").await?;
  assert_eq!(foo, "123");
  let foo: Vec<u8> = client.get("foo").await?;
  assert_eq!(foo, "123".as_bytes());
  let foo: Vec<String> = client.hvals("bar").await?;
  assert_eq!(array_to_set(foo), array_to_set(vec!["1".to_owned(), "2".to_owned()]));
  let foo: BTreeSet<String> = client.hvals("bar").await?;
  assert_eq!(foo, array_to_set(vec!["1".to_owned(), "2".to_owned()]));
  let foo: HashMap<String, u16> = client.hgetall("bar").await?;
  assert_eq!(foo, Value::Map(nested_values.clone()).convert()?);
  let foo: BTreeMap<Key, u16> = client.hgetall("bar").await?;
  assert_eq!(foo, hash_to_btree(&nested_values));
  let foo: (String, i64) = client.mget(vec!["foo", "baz"]).await?;
  assert_eq!(foo, ("123".into(), 456));
  let foo: Vec<(String, i64)> = client.hgetall("bar").await?;
  assert_eq!(array_to_set(foo), array_to_set(vec![("a".into(), 1), ("b".into(), 2)]));

  Ok(())
}

#[cfg(all(feature = "i-client", feature = "i-lists"))]
pub async fn should_automatically_unblock(_: Client, mut config: Config) -> Result<(), Error> {
  config.blocking = Blocking::Interrupt;
  let client = Client::new(config, None, None, None);
  client.connect();
  client.wait_for_connect().await?;

  let unblock_client = client.clone();
  tokio::spawn(async move {
    sleep(Duration::from_secs(1)).await;
    let _: () = unblock_client.ping(None).await.expect("Failed to ping");
  });

  let result = client.blpop::<(), _>("foo", 60.0).await;
  assert!(result.is_err());
  assert_ne!(*result.unwrap_err().kind(), ErrorKind::Timeout);
  Ok(())
}

#[cfg(all(feature = "i-client", feature = "i-lists"))]
pub async fn should_manually_unblock(client: Client, _: Config) -> Result<(), Error> {
  let connections_ids = client.connection_ids();
  let unblock_client = client.clone();

  tokio::spawn(async move {
    sleep(Duration::from_secs(1)).await;

    for (_, id) in connections_ids.into_iter() {
      let _ = unblock_client
        .client_unblock::<(), _>(id, Some(ClientUnblockFlag::Error))
        .await;
    }
  });

  let result = client.blpop::<(), _>("foo", 60.0).await;
  assert!(result.is_err());
  assert_ne!(*result.unwrap_err().kind(), ErrorKind::Timeout);
  Ok(())
}

#[cfg(all(feature = "i-client", feature = "i-lists"))]
pub async fn should_error_when_blocked(_: Client, mut config: Config) -> Result<(), Error> {
  config.blocking = Blocking::Error;
  let client = Client::new(config, None, None, None);
  client.connect();
  client.wait_for_connect().await?;
  let error_client = client.clone();

  tokio::spawn(async move {
    sleep(Duration::from_secs(1)).await;

    let result = error_client.ping::<()>(None).await;
    assert!(result.is_err());
    assert_eq!(*result.unwrap_err().kind(), ErrorKind::InvalidCommand);

    let _ = error_client.unblock_self(None).await;
  });

  let result = client.blpop::<(), _>("foo", 60.0).await;
  assert!(result.is_err());
  Ok(())
}

pub async fn should_split_clustered_connection(client: Client, _config: Config) -> Result<(), Error> {
  let actual = client
    .split_cluster()?
    .iter()
    .map(|client| client.client_config())
    .fold(BTreeSet::new(), |mut set, config| {
      if let ServerConfig::Centralized { server } = config.server {
        set.insert(server);
      } else {
        panic!("expected centralized config");
      }

      set
    });

  assert_eq!(actual.len(), 3);
  Ok(())
}

#[cfg(feature = "metrics")]
pub async fn should_track_size_stats(client: Client, _config: Config) -> Result<(), Error> {
  let _ = client.take_res_size_metrics();
  let _ = client.take_req_size_metrics();

  let _: () = client
    .set("foo", "abcdefghijklmnopqrstuvxyz", None, None, false)
    .await?;
  let req_stats = client.take_req_size_metrics();
  let res_stats = client.take_res_size_metrics();

  // manually calculated with the redis_protocol crate `encode` function (not shown here)
  let expected_req_size = 54;
  let expected_res_size = 5;

  assert_eq!(req_stats.sum, expected_req_size);
  assert_eq!(req_stats.samples, 1);
  assert_eq!(res_stats.sum, expected_res_size);
  assert_eq!(res_stats.samples, 1);

  Ok(())
}

#[cfg(feature = "i-server")]
pub async fn should_run_flushall_cluster(client: Client, _: Config) -> Result<(), Error> {
  let count: i64 = 200;

  for idx in 0 .. count {
    let _: () = client
      .custom(cmd!("SET"), vec![format!("foo-{}", idx), idx.to_string()])
      .await?;
  }
  client.flushall_cluster().await?;

  for idx in 0 .. count {
    let value: Option<i64> = client.custom(cmd!("GET"), vec![format!("foo-{}", idx)]).await?;
    assert!(value.is_none());
  }

  Ok(())
}

pub async fn should_safely_change_protocols_repeatedly(client: Client, _: Config) -> Result<(), Error> {
  let done = Arc::new(RwLock::new(false));
  let other = client.clone();
  let other_done = done.clone();

  let jh = tokio::spawn(async move {
    loop {
      if *other_done.read() {
        return Ok::<_, Error>(());
      }
      let _: () = other.ping(None).await?;
      sleep(Duration::from_millis(10)).await;
    }
  });

  for idx in 0 .. 20 {
    let version = if idx % 2 == 0 {
      RespVersion::RESP2
    } else {
      RespVersion::RESP3
    };
    client.hello(version, None, None).await?;
    sleep(Duration::from_millis(100)).await;
  }
  let _ = mem::replace(&mut *done.write(), true);

  let _ = jh.await?;
  Ok(())
}

// test to repro an intermittent race condition found while stress testing the client
#[allow(dead_code)]
#[cfg(feature = "i-keys")]
pub async fn should_test_high_concurrency_pool(_: Client, mut config: Config) -> Result<(), Error> {
  config.blocking = Blocking::Block;
  let perf = PerformanceConfig::default();
  let pool = Pool::new(config, Some(perf), None, None, 28)?;
  pool.connect();
  pool.wait_for_connect().await?;

  let num_tasks = 11641;
  let mut tasks = Vec::with_capacity(num_tasks);
  let counter = Arc::new(AtomicUsize::new(0));

  for idx in 0 .. num_tasks {
    let client = pool.next().clone();
    let counter = counter.clone();

    tasks.push(tokio::spawn(async move {
      let key = format!("foo-{}", idx);

      let mut expected = 0;
      while incr_atomic(&counter) < 50_000_000 {
        let actual: i64 = client.incr(&key).await?;
        expected += 1;
        if actual != expected {
          return Err(Error::new(
            ErrorKind::Unknown,
            format!("Expected {}, found {}", expected, actual),
          ));
        }
      }

      // println!("Task {} finished.", idx);
      Ok::<_, Error>(())
    }));
  }
  let _ = futures::future::try_join_all(tasks).await?;

  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_pipeline_all(client: Client, _: Config) -> Result<(), Error> {
  let pipeline = client.pipeline();

  let result: Value = pipeline.set("foo", 1, None, None, false).await?;
  assert!(result.is_queued());
  let result: Value = pipeline.set("bar", 2, None, None, false).await?;
  assert!(result.is_queued());
  let result: Value = pipeline.incr("foo").await?;
  assert!(result.is_queued());

  let result: ((), (), i64) = pipeline.all().await?;
  assert_eq!(result.2, 2);
  Ok(())
}

#[cfg(all(feature = "i-keys", feature = "i-hashes"))]
pub async fn should_pipeline_all_error_early(client: Client, _: Config) -> Result<(), Error> {
  let pipeline = client.pipeline();

  let result: Value = pipeline.set("foo", 1, None, None, false).await?;
  assert!(result.is_queued());
  let result: Value = pipeline.hgetall("foo").await?;
  assert!(result.is_queued());
  let result: Value = pipeline.incr("foo").await?;
  assert!(result.is_queued());

  if let Err(e) = pipeline.all::<Value>().await {
    // make sure we get the expected error from the server rather than a parsing error
    assert_eq!(*e.kind(), ErrorKind::InvalidArgument);
  } else {
    panic!("Expected pipeline error.");
  }

  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_pipeline_last(client: Client, _: Config) -> Result<(), Error> {
  let pipeline = client.pipeline();

  let result: Value = pipeline.set("foo", 1, None, None, false).await?;
  assert!(result.is_queued());
  let result: Value = pipeline.set("bar", 2, None, None, false).await?;
  assert!(result.is_queued());
  let result: Value = pipeline.incr("foo").await?;
  assert!(result.is_queued());

  let result: i64 = pipeline.last().await?;
  assert_eq!(result, 2);
  Ok(())
}

#[cfg(all(feature = "i-keys", feature = "i-hashes"))]
pub async fn should_pipeline_try_all(client: Client, _: Config) -> Result<(), Error> {
  let pipeline = client.pipeline();

  let _: () = pipeline.incr("foo").await?;
  let _: () = pipeline.hgetall("foo").await?;
  let results = pipeline.try_all::<i64>().await;

  assert_eq!(results[0].clone().unwrap(), 1);
  assert!(results[1].is_err());

  Ok(())
}

#[cfg(feature = "i-server")]
pub async fn should_use_all_cluster_nodes_repeatedly(client: Client, _: Config) -> Result<(), Error> {
  let other = client.clone();
  let jh1 = tokio::spawn(async move {
    for _ in 0 .. 200 {
      other.flushall_cluster().await?;
    }

    Ok::<_, Error>(())
  });
  let jh2 = tokio::spawn(async move {
    for _ in 0 .. 200 {
      client.flushall_cluster().await?;
    }

    Ok::<_, Error>(())
  });

  let _ = try_join(jh1, jh2).await?;
  Ok(())
}

#[cfg(all(feature = "partial-tracing", feature = "i-keys"))]
pub async fn should_use_tracing_get_set(client: Client, mut config: Config) -> Result<(), Error> {
  config.tracing = TracingConfig::new(true);
  let (perf, policy) = (client.perf_config(), client.client_reconnect_policy());
  let client = Client::new(config, Some(perf), None, policy);
  let _ = client.connect();
  let _ = client.wait_for_connect().await?;

  let _: () = client.set("foo", "bar", None, None, false).await?;
  assert_eq!(client.get::<String, _>("foo").await?, "bar");
  Ok(())
}

// #[cfg(feature = "dns")]
// pub struct TrustDnsResolver(TokioAsyncResolver);
//
// #[cfg(feature = "dns")]
// impl TrustDnsResolver {
// fn new() -> Self {
// TrustDnsResolver(TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default()).unwrap())
// }
// }
//
// #[cfg(feature = "dns")]
// #[async_trait]
// impl Resolve for TrustDnsResolver {
// async fn resolve(&self, host: String, port: u16) -> Result<SocketAddr, RedisError> {
// println!("Looking up {}", host);
// self.0.lookup_ip(&host).await.map_err(|e| e.into()).and_then(|ips| {
// let ip = match ips.iter().next() {
// Some(ip) => ip,
// None => return Err(RedisError::new(RedisErrorKind::IO, "Failed to lookup IP address.")),
// };
//
// debug!("Mapped {}:{} to {}:{}", host, port, ip, port);
// Ok(SocketAddr::new(ip, port))
// })
// }
// }
//
// #[cfg(feature = "dns")]
// TODO fix the DNS configuration in docker so trustdns works
// pub async fn should_use_trust_dns(client: RedisClient, mut config: RedisConfig) -> Result<(), RedisError> {
// let perf = client.perf_config();
// let policy = client.client_reconnect_policy();
//
// if let ServerConfig::Centralized { ref mut host, .. } = config.server {
// host = utils::read_redis_centralized_host().0;
// }
// if let ServerConfig::Clustered { ref mut hosts } = config.server {
// hosts[0].0 = utils::read_redis_cluster_host().0;
// }
//
// println!("Trust DNS host: {:?}", config.server.hosts());
// let client = RedisClient::new(config, Some(perf), policy);
// client.set_resolver(Arc::new(TrustDnsResolver::new())).await;
//
// let _ = client.connect();
// let _ = client.wait_for_connect().await?;
// let _: () = client.ping().await?;
// let _ = client.quit().await?;
// Ok(())
// }

#[cfg(feature = "subscriber-client")]
pub async fn should_ping_with_subscriber_client(client: Client, config: Config) -> Result<(), Error> {
  let (perf, policy) = (client.perf_config(), client.client_reconnect_policy());
  let client = SubscriberClient::new(config, Some(perf), None, policy);
  let _ = client.connect();
  let _ = client.wait_for_connect().await?;

  let _: () = client.ping(None).await?;
  let _: () = client.subscribe("foo").await?;
  let _: () = client.ping(None).await?;
  let _ = client.quit().await?;
  Ok(())
}

#[cfg(all(feature = "replicas", feature = "i-keys"))]
pub async fn should_replica_set_and_get(client: Client, config: Config) -> Result<(), Error> {
  let policy = client.client_reconnect_policy();
  let mut connection = client.connection_config().clone();
  connection.replica = ReplicaConfig::default();
  let client = Client::new(config, None, Some(connection), policy);
  client.init().await?;

  let _: () = client.set("foo", "bar", None, None, false).await?;
  let result: String = client.replicas().get("foo").await?;
  assert_eq!(result, "bar");

  Ok(())
}

#[cfg(all(feature = "replicas", feature = "i-keys"))]
pub async fn should_replica_set_and_get_not_lazy(client: Client, config: Config) -> Result<(), Error> {
  let policy = client.client_reconnect_policy();
  let mut connection = client.connection_config().clone();
  connection.replica.lazy_connections = false;
  let client = Client::new(config, None, Some(connection), policy);
  client.init().await?;

  let _: () = client.set("foo", "bar", None, None, false).await?;
  let result: String = client.replicas().get("foo").await?;
  assert_eq!(result, "bar");

  Ok(())
}

#[cfg(all(feature = "replicas", feature = "i-keys"))]
pub async fn should_pipeline_with_replicas(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.set("foo", 1, None, None, false).await?;
  let _: () = client.set("bar", 2, None, None, false).await?;

  let pipeline = client.replicas().pipeline();
  let _: () = pipeline.get("foo").await?;
  let _: () = pipeline.get("bar").await?;
  let result: (i64, i64) = pipeline.all().await?;

  assert_eq!(result, (1, 2));
  Ok(())
}

#[cfg(all(feature = "replicas", feature = "i-keys"))]
pub async fn should_use_cluster_replica_without_redirection(client: Client, config: Config) -> Result<(), Error> {
  let mut connection = client.connection_config().clone();
  connection.replica = ReplicaConfig {
    lazy_connections: true,
    primary_fallback: false,
    ignore_reconnection_errors: true,
    ..ReplicaConfig::default()
  };
  connection.max_redirections = 0;
  let policy = client.client_reconnect_policy();

  let client = Client::new(config, None, Some(connection), policy);
  let _ = client.connect();
  client.wait_for_connect().await?;

  let _: () = client.replicas().get("foo").await?;
  let _: () = client.incr("foo").await?;

  Ok(())
}

pub async fn should_gracefully_quit(client: Client, _: Config) -> Result<(), Error> {
  let client = client.clone_new();
  let connection = client.connect();
  client.wait_for_connect().await?;

  let _: () = client.ping(None).await?;
  let _: () = client.quit().await?;
  let _ = connection.await;

  Ok(())
}

#[cfg(feature = "i-lists")]
pub async fn should_support_options_with_pipeline(client: Client, _: Config) -> Result<(), Error> {
  let options = Options {
    timeout: Some(Duration::from_millis(100)),
    max_attempts: Some(42),
    max_redirections: Some(43),
    ..Default::default()
  };

  let pipeline = client.pipeline().with_options(&options);
  let _: () = pipeline.blpop("foo", 2.0).await?;
  let results = pipeline.try_all::<Value>().await;
  assert_eq!(results[0].clone().unwrap_err().kind(), &ErrorKind::Timeout);

  Ok(())
}

#[cfg(feature = "i-keys")]
pub async fn should_reuse_pipeline(client: Client, _: Config) -> Result<(), Error> {
  let pipeline = client.pipeline();
  let _: () = pipeline.incr("foo").await?;
  let _: () = pipeline.incr("foo").await?;
  assert_eq!(pipeline.last::<i64>().await?, 2);
  assert_eq!(pipeline.last::<i64>().await?, 4);
  Ok(())
}

#[cfg(all(feature = "transactions", feature = "i-keys"))]
pub async fn should_support_options_with_trx(client: Client, _: Config) -> Result<(), Error> {
  let options = Options {
    max_attempts: Some(1),
    timeout: Some(Duration::from_secs(1)),
    ..Default::default()
  };
  let trx = client.multi().with_options(&options);

  let _: () = trx.get("foo{1}").await?;
  let _: () = trx.set("foo{1}", "bar", None, None, false).await?;
  let _: () = trx.get("foo{1}").await?;
  let (first, second, third): (Option<Value>, bool, String) = trx.exec(true).await?;

  assert_eq!(first, None);
  assert!(second);
  assert_eq!(third, "bar");
  Ok(())
}

#[cfg(all(feature = "transactions", feature = "i-keys"))]
pub async fn should_pipeline_transaction(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.incr("foo{1}").await?;
  let _: () = client.incr("bar{1}").await?;

  let trx = client.multi();
  let _: () = trx.get("foo{1}").await?;
  let _: () = trx.incr("bar{1}").await?;
  let (foo, bar): (i64, i64) = trx.exec(true).await?;
  assert_eq!((foo, bar), (1, 2));

  Ok(())
}

#[cfg(all(feature = "transactions", feature = "i-keys", feature = "i-hashes"))]
pub async fn should_fail_pipeline_transaction_error(client: Client, _: Config) -> Result<(), Error> {
  let _: () = client.incr("foo{1}").await?;
  let _: () = client.incr("bar{1}").await?;

  let trx = client.multi();
  let _: () = trx.get("foo{1}").await?;
  let _: () = trx.hgetall("bar{1}").await?;
  let _: () = trx.get("foo{1}").await?;

  if let Err(e) = trx.exec::<Value>(false).await {
    assert_eq!(*e.kind(), ErrorKind::InvalidArgument);
  } else {
    panic!("Expected error from transaction.");
  }

  Ok(())
}

#[cfg(all(feature = "i-keys", feature = "i-lists"))]
pub async fn should_manually_connect_twice(client: Client, _: Config) -> Result<(), Error> {
  let client = client.clone_new();
  let _old_connection = client.connect();
  client.wait_for_connect().await?;

  let _blpop_jh = tokio::spawn({
    let client = client.clone();
    async move { client.blpop::<Option<i64>, _>("foo", 5.0).await }
  });

  sleep(Duration::from_millis(100)).await;
  let new_connection = client.connect();
  client.wait_for_connect().await?;

  assert_eq!(client.incr::<i64, _>("bar").await?, 1);
  client.quit().await?;
  let _ = new_connection.await?;
  Ok(())
}

pub async fn pool_should_connect_correctly_via_init_interface(_: Client, config: Config) -> Result<(), Error> {
  let pool = Builder::from_config(config).build_pool(5)?;
  let task = pool.init().await?;

  let _: () = pool.ping(None).await?;
  let _: () = pool.quit().await?;
  task.await??;
  Ok(())
}

pub async fn pool_should_fail_with_bad_host_via_init_interface(_: Client, mut config: Config) -> Result<(), Error> {
  config.fail_fast = true;
  config.server = ServerConfig::new_centralized("incorrecthost", 1234);
  let pool = Builder::from_config(config).build_pool(5)?;
  assert!(pool.init().await.is_err());
  Ok(())
}

pub async fn pool_should_connect_correctly_via_wait_interface(_: Client, config: Config) -> Result<(), Error> {
  let pool = Builder::from_config(config).build_pool(5)?;
  let task = pool.connect();
  pool.wait_for_connect().await?;

  let _: () = pool.ping(None).await?;
  let _: () = pool.quit().await?;
  task.await??;
  Ok(())
}

pub async fn pool_should_fail_with_bad_host_via_wait_interface(_: Client, mut config: Config) -> Result<(), Error> {
  config.fail_fast = true;
  config.server = ServerConfig::new_centralized("incorrecthost", 1234);
  let pool = Builder::from_config(config).build_pool(5)?;
  let task = pool.connect();
  assert!(pool.wait_for_connect().await.is_err());

  let _ = task.await;
  Ok(())
}

pub async fn should_connect_correctly_via_init_interface(_: Client, config: Config) -> Result<(), Error> {
  let client = Builder::from_config(config).build()?;
  let task = client.init().await?;

  let _: () = client.ping(None).await?;
  let _: () = client.quit().await?;
  task.await??;
  Ok(())
}

pub async fn should_fail_with_bad_host_via_init_interface(_: Client, mut config: Config) -> Result<(), Error> {
  config.fail_fast = true;
  config.server = ServerConfig::new_centralized("incorrecthost", 1234);
  let client = Builder::from_config(config).build()?;
  assert!(client.init().await.is_err());
  Ok(())
}

pub async fn should_connect_correctly_via_wait_interface(_: Client, config: Config) -> Result<(), Error> {
  let client = Builder::from_config(config).build()?;
  let task = client.connect();
  client.wait_for_connect().await?;

  let _: () = client.ping(None).await?;
  let _: () = client.quit().await?;
  task.await??;
  Ok(())
}

pub async fn should_fail_with_bad_host_via_wait_interface(_: Client, mut config: Config) -> Result<(), Error> {
  config.fail_fast = true;
  config.server = ServerConfig::new_centralized("incorrecthost", 1234);
  let client = Builder::from_config(config).build()?;
  let task = client.connect();
  assert!(client.wait_for_connect().await.is_err());

  let _ = task.await;
  Ok(())
}

#[cfg(all(feature = "replicas", feature = "i-keys"))]
pub async fn should_combine_options_and_replicas(client: Client, config: Config) -> Result<(), Error> {
  let mut connection = client.connection_config().clone();
  connection.replica = ReplicaConfig {
    lazy_connections: true,
    primary_fallback: false,
    ignore_reconnection_errors: true,
    ..ReplicaConfig::default()
  };
  connection.max_redirections = 0;
  let policy = client.client_reconnect_policy();
  let client = Client::new(config, None, Some(connection), policy);
  client.init().await?;

  // change the cluster hash policy such that we get a routing error if both replicas and options are correctly
  // applied
  let key = Key::from_static_str("foo");
  let (servers, foo_owner) = client
    .cached_cluster_state()
    .map(|s| {
      (
        s.unique_primary_nodes(),
        s.get_server(key.cluster_hash()).unwrap().clone(),
      )
    })
    .unwrap();
  // in this case the caller has specified the wrong cluster owner node, and none of the replica connections have been
  // created since lazy_connections is true. the client should check whether the provided node matches the primary or
  // any of the replicas, and if not it should return an error early that the command is not routable.
  let wrong_owner = servers.iter().find(|s| foo_owner != **s).unwrap().clone();

  let options = Options {
    max_redirections: Some(0),
    max_attempts: Some(1),
    cluster_node: Some(wrong_owner),
    ..Default::default()
  };

  let error = client
    .with_options(&options)
    .replicas()
    .get::<Option<String>, _>(key)
    .await
    .err()
    .unwrap();

  assert_eq!(*error.kind(), ErrorKind::Routing);
  Ok(())
}

#[cfg(all(feature = "replicas", feature = "i-keys"))]
pub async fn should_combine_options_and_replicas_non_lazy(client: Client, config: Config) -> Result<(), Error> {
  let mut connection = client.connection_config().clone();
  connection.replica = ReplicaConfig {
    lazy_connections: false,
    primary_fallback: false,
    ignore_reconnection_errors: false,
    ..ReplicaConfig::default()
  };
  connection.max_redirections = 0;
  let policy = client.client_reconnect_policy();
  let client = Client::new(config, None, Some(connection), policy);
  client.init().await?;

  // change the cluster hash policy such that we get a routing error if both replicas and options are correctly
  // applied
  let key = Key::from_static_str("foo");
  let (servers, foo_owner) = client
    .cached_cluster_state()
    .map(|s| {
      (
        s.unique_primary_nodes(),
        s.get_server(key.cluster_hash()).unwrap().clone(),
      )
    })
    .unwrap();
  // in this case since all the connections are created the client will route to a replica of the wrong primary node,
  // receiving a MOVED redirection in response. since the max redirections is zero the client should return a "too
  // many redirections" error.
  let wrong_owner = servers.iter().find(|s| foo_owner != **s).unwrap().clone();

  let options = Options {
    max_redirections: Some(0),
    max_attempts: Some(1),
    cluster_node: Some(wrong_owner),
    ..Default::default()
  };

  let error = client
    .with_options(&options)
    .replicas()
    .get::<Option<String>, _>(key)
    .await
    .err()
    .unwrap();

  assert_eq!(*error.kind(), ErrorKind::Routing);
  Ok(())
}

pub async fn should_fail_on_centralized_connect(_: Client, mut config: Config) -> Result<(), Error> {
  if let ServerConfig::Centralized { server } = config.server {
    config.server = ServerConfig::Clustered {
      hosts:  vec![server],
      policy: ClusterDiscoveryPolicy::default(),
    };
  } else {
    // skip for unix socket and sentinel tests
    return Ok(());
  }

  let client = Client::new(config, None, None, None);
  client.connect();

  if let Err(err) = client.wait_for_connect().await {
    assert_eq!(*err.kind(), ErrorKind::Config, "err = {:?}", err);
    return Ok(());
  }

  Err(Error::new(ErrorKind::Unknown, "Expected a config error."))
}

#[derive(Debug, Default)]
#[cfg(feature = "credential-provider")]
pub struct FakeCreds {}

#[async_trait]
#[cfg(feature = "credential-provider")]
impl CredentialProvider for FakeCreds {
  async fn fetch(&self, _: Option<&Server>) -> Result<(Option<String>, Option<String>), Error> {
    use super::utils::{read_redis_password, read_redis_username};
    Ok((Some(read_redis_username()), Some(read_redis_password())))
  }
}
#[cfg(feature = "credential-provider")]
pub async fn should_use_credential_provider(_client: Client, mut config: Config) -> Result<(), Error> {
  let (perf, connection) = (_client.perf_config(), _client.connection_config().clone());
  config.username = None;
  config.password = None;
  config.credential_provider = Some(Arc::new(FakeCreds::default()));
  let client = Builder::from_config(config)
    .set_connection_config(connection)
    .set_performance_config(perf)
    .build()?;

  client.init().await?;
  let _: () = client.ping(None).await?;
  let _: () = client.quit().await?;
  Ok(())
}

#[cfg(feature = "i-pubsub")]
pub async fn should_exit_event_task_with_error(client: Client, _: Config) -> Result<(), Error> {
  let task = client.on_message(|_| async { Err(Error::new_canceled()) });
  let _: () = client.subscribe("foo").await?;

  let publisher = client.clone_new();
  publisher.init().await?;
  let _: () = publisher.publish("foo", "bar").await?;

  let result = task.await.unwrap();
  assert_eq!(result, Err(Error::new_canceled()));
  Ok(())
}

#[cfg(feature = "replicas")]
pub async fn should_create_non_lazy_replica_connections(client: Client, config: Config) -> Result<(), Error> {
  if !config.server.is_clustered() {
    return Ok(());
  }

  let mut connection_config = client.connection_config().clone();
  connection_config.replica = ReplicaConfig {
    lazy_connections: false,
    primary_fallback: true,
    ..Default::default()
  };

  let client = Builder::from_config(config)
    .set_performance_config(client.perf_config())
    .set_connection_config(connection_config)
    .build()?;
  client.init().await?;

  assert_eq!(client.active_connections().len(), 6);
  Ok(())
}

#[cfg(all(feature = "transactions", feature = "i-keys"))]
pub async fn should_mix_trx_and_get(client: Client, _: Config) -> Result<(), Error> {
  let mut set = JoinSet::new();
  for _ in 0 .. 200 {
    let client = client.clone();
    set.spawn(async move {
      let tx = client.multi();
      let _: () = tx.incr("foo").await.unwrap();
      let _: () = tx.exec(true).await.unwrap();
      let _: () = client.get("bar").await.unwrap();
    });
  }

  set.join_all().await;
  Ok(())
}

pub async fn should_not_hang_on_concurrent_quit(client: Client, _: Config) -> Result<(), Error> {
  let client2 = client.clone();

  let task1 = tokio::spawn(async move { client.quit().await });
  let task2 = tokio::spawn(async move { client2.quit().await });
  task1.await.unwrap()?;
  task2.await.unwrap()?;
  Ok(())
}
