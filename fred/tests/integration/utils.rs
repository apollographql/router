#![allow(unused_macros)]
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(clippy::match_like_matches_macro)]

use fred::{
  clients::Client,
  error::Error,
  interfaces::*,
  types::{
    config::{
      ClusterDiscoveryPolicy,
      Config,
      ConnectionConfig,
      PerformanceConfig,
      ReconnectPolicy,
      Server,
      ServerConfig,
      UnresponsiveConfig,
    },
    Builder,
    ConnectHandle,
    InfoKind,
  },
};
use redis_protocol::resp3::types::RespVersion;
use std::{
  convert::TryInto,
  default::Default,
  env,
  fmt,
  fmt::{Debug, Formatter},
  fs,
  future::Future,
  time::Duration,
};

const RECONNECT_DELAY: u32 = 1000;

#[cfg(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
))]
use fred::types::config::{TlsConfig, TlsConnector, TlsHostMapping};
#[cfg(feature = "enable-native-tls")]
use tokio_native_tls::native_tls::{
  Certificate as NativeTlsCertificate,
  Identity,
  TlsConnector as NativeTlsConnector,
};
#[cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))]
use tokio_rustls::rustls::{ClientConfig, ConfigBuilder, RootCertStore, WantsVerifier};

pub fn read_env_var(name: &str) -> Option<String> {
  env::var_os(name).and_then(|s| s.into_string().ok())
}

pub fn should_use_sentinel_config() -> bool {
  read_env_var("FRED_SENTINEL_TESTS")
    .map(|s| match s.as_ref() {
      "1" | "t" | "true" | "yes" => true,
      _ => false,
    })
    .unwrap_or(false)
}

pub fn should_flushall_between_tests() -> bool {
  read_env_var("FRED_NO_FLUSHALL_DURING_TESTS")
    .map(|s| match s.as_ref() {
      "1" | "t" | "true" | "yes" => false,
      _ => true,
    })
    .unwrap_or(true)
}

pub fn read_ci_tls_env() -> bool {
  match env::var_os("FRED_CI_TLS") {
    Some(s) => match s.into_string() {
      Ok(s) => match s.as_ref() {
        "t" | "true" | "TRUE" | "1" => true,
        _ => false,
      },
      Err(_) => false,
    },
    None => false,
  }
}

fn read_fail_fast_env() -> bool {
  match env::var_os("FRED_FAIL_FAST") {
    Some(s) => match s.into_string() {
      Ok(s) => match s.as_ref() {
        "f" | "false" | "FALSE" | "0" => false,
        _ => true,
      },
      Err(_) => true,
    },
    None => true,
  }
}

#[cfg(feature = "i-redis-stack")]
pub fn read_redis_centralized_host() -> (String, u16) {
  let host = read_env_var("FRED_REDIS_STACK_HOST").unwrap_or("redis-main".into());
  let port = read_env_var("FRED_REDIS_STACK_PORT")
    .and_then(|s| s.parse::<u16>().ok())
    .unwrap_or(6379);

  (host, port)
}

#[cfg(not(feature = "i-redis-stack"))]
pub fn read_redis_centralized_host() -> (String, u16) {
  let host = read_env_var("FRED_REDIS_CENTRALIZED_HOST").unwrap_or("redis-main".into());
  let port = read_env_var("FRED_REDIS_CENTRALIZED_PORT")
    .and_then(|s| s.parse::<u16>().ok())
    .unwrap_or(6379);

  (host, port)
}

#[cfg(not(any(
  feature = "enable-native-tls",
  feature = "enable-rustls",
  feature = "enable-rustls-ring"
)))]
pub fn read_redis_cluster_host() -> (String, u16) {
  let host = read_env_var("FRED_REDIS_CLUSTER_HOST").unwrap_or("redis-cluster-1".into());
  let port = read_env_var("FRED_REDIS_CLUSTER_PORT")
    .and_then(|s| s.parse::<u16>().ok())
    .unwrap_or(30001);

  (host, port)
}

#[cfg(any(
  feature = "enable-native-tls",
  feature = "enable-rustls",
  feature = "enable-rustls-ring"
))]
pub fn read_redis_cluster_host() -> (String, u16) {
  let host = read_env_var("FRED_REDIS_CLUSTER_TLS_HOST").unwrap_or("redis-cluster-tls-1".into());
  let port = read_env_var("FRED_REDIS_CLUSTER_TLS_PORT")
    .and_then(|s| s.parse::<u16>().ok())
    .unwrap_or(40001);

  (host, port)
}

pub fn read_redis_password() -> String {
  read_env_var("REDIS_PASSWORD").expect("Failed to read REDIS_PASSWORD env")
}

#[cfg(not(feature = "i-redis-stack"))]
pub fn read_redis_username() -> String {
  read_env_var("REDIS_USERNAME").expect("Failed to read REDIS_USERNAME env")
}

// the CI settings for redis-stack don't set up custom ACL rules
#[cfg(feature = "i-redis-stack")]
pub fn read_redis_username() -> String {
  read_env_var("REDIS_USERNAME").unwrap_or("default".into())
}

#[cfg(feature = "sentinel-auth")]
pub fn read_sentinel_password() -> String {
  read_env_var("REDIS_SENTINEL_PASSWORD").expect("Failed to read REDIS_SENTINEL_PASSWORD env")
}

#[cfg(feature = "unix-sockets")]
pub fn read_unix_socket_path() -> String {
  let dir = read_env_var("REDIS_UNIX_SOCK_CONTAINER_DIR").expect("Failed to read REDIS_UNIX_SOCK_CONTAINER_DIR");
  let sock = read_env_var("REDIS_UNIX_SOCK").expect("Failed to read REDIS_UNIX_SOCK");
  format!("{}/{}", dir, sock)
}

pub fn read_sentinel_server() -> (String, u16) {
  let host = read_env_var("FRED_REDIS_SENTINEL_HOST").unwrap_or("127.0.0.1".into());
  let port = read_env_var("FRED_REDIS_SENTINEL_PORT")
    .and_then(|s| s.parse::<u16>().ok())
    .unwrap_or(26379);

  (host, port)
}

#[cfg(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
))]
#[allow(dead_code)]
struct TlsCreds {
  root_cert_der:   Vec<u8>,
  root_cert_pem:   Vec<u8>,
  client_cert_der: Vec<u8>,
  client_cert_pem: Vec<u8>,
  client_key_der:  Vec<u8>,
  client_key_pem:  Vec<u8>,
}

#[cfg(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
))]
fn check_file_contents(value: &[u8], msg: &str) {
  if value.is_empty() {
    panic!("Invalid empty TLS file: {}", msg);
  }
}

/// Read the CA cert, client cert, and client key from the Redis tests TLS directory.
#[cfg(any(
  feature = "enable-native-tls",
  feature = "enable-rustls",
  feature = "enable-rustls-ring"
))]
fn read_tls_creds() -> TlsCreds {
  let creds_path = read_env_var("FRED_TEST_TLS_CREDS").expect("Failed to read TLS path from env");
  let root_cert_pem_path = format!("{}/ca.crt", creds_path);
  let root_cert_der_path = format!("{}/ca.der", creds_path);
  let client_cert_pem_path = format!("{}/client.crt", creds_path);
  let client_cert_der_path = format!("{}/client.der", creds_path);
  let client_key_pem_path = format!("{}/client.key8", creds_path);
  let client_key_der_path = format!("{}/client.key8_der", creds_path);

  let root_cert_pem = fs::read(&root_cert_pem_path).expect("Failed to read root cert pem");
  let root_cert_der = fs::read(&root_cert_der_path).expect("Failed to read root cert der");
  let client_cert_pem = fs::read(&client_cert_pem_path).expect("Failed to read client cert pem");
  let client_cert_der = fs::read(&client_cert_der_path).expect("Failed to read client cert der");
  let client_key_der = fs::read(&client_key_der_path).expect("Failed to read client key der");
  let client_key_pem = fs::read(&client_key_pem_path).expect("Failed to read client key pem");

  check_file_contents(&root_cert_pem, "root cert pem");
  check_file_contents(&root_cert_der, "root cert der");
  check_file_contents(&client_cert_pem, "client cert pem");
  check_file_contents(&client_cert_der, "client cert der");
  check_file_contents(&client_key_pem, "client key pem");
  check_file_contents(&client_key_der, "client key der");

  TlsCreds {
    root_cert_pem,
    root_cert_der,
    client_cert_der,
    client_cert_pem,
    client_key_pem,
    client_key_der,
  }
}

#[cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))]
fn create_rustls_config() -> TlsConnector {
  use rustls::pki_types::PrivatePkcs8KeyDer;

  let creds = read_tls_creds();
  let mut root_store = RootCertStore::empty();
  root_store
    .add(creds.root_cert_der.clone().into())
    .expect("Failed adding to rustls root cert store");

  let cert_chain = vec![creds.client_cert_der.into(), creds.root_cert_der.into()];

  ClientConfig::builder()
    .with_root_certificates(root_store)
    .with_client_auth_cert(cert_chain, PrivatePkcs8KeyDer::from(creds.client_key_der).into())
    .expect("Failed to build rustls client config")
    .into()
}

#[cfg(feature = "enable-native-tls")]
fn create_native_tls_config() -> TlsConnector {
  let creds = read_tls_creds();

  let root_cert = NativeTlsCertificate::from_pem(&creds.root_cert_pem).expect("Failed to parse root cert");
  let mut builder = NativeTlsConnector::builder();
  builder.add_root_certificate(root_cert);

  let mut client_cert_chain = Vec::with_capacity(creds.client_cert_pem.len() + creds.root_cert_pem.len());
  client_cert_chain.extend(&creds.client_cert_pem);
  client_cert_chain.extend(&creds.root_cert_pem);

  let identity =
    Identity::from_pkcs8(&client_cert_chain, &creds.client_key_pem).expect("Failed to create client identity");
  builder.identity(identity);

  builder.try_into().expect("Failed to build native-tls connector")
}

fn reconnect_settings() -> (Option<ReconnectPolicy>, u32, bool) {
  (Some(ReconnectPolicy::new_constant(300, RECONNECT_DELAY)), 3, true)
}

#[cfg(feature = "unix-sockets")]
fn create_server_config(cluster: bool) -> ServerConfig {
  ServerConfig::Unix {
    path: read_unix_socket_path().into(),
  }
}

#[cfg(not(feature = "unix-sockets"))]
fn create_server_config(cluster: bool) -> ServerConfig {
  if cluster {
    let (host, port) = read_redis_cluster_host();
    ServerConfig::Clustered {
      hosts:  vec![Server::new(host, port)],
      policy: ClusterDiscoveryPolicy::default(),
    }
  } else {
    let (host, port) = read_redis_centralized_host();
    ServerConfig::Centralized {
      server: Server::new(host, port),
    }
  }
}

fn create_normal_redis_config(cluster: bool, resp3: bool) -> (Config, PerformanceConfig) {
  let config = Config {
    fail_fast: read_fail_fast_env(),
    server: create_server_config(cluster),
    version: if resp3 { RespVersion::RESP3 } else { RespVersion::RESP2 },
    username: Some(read_redis_username()),
    password: Some(read_redis_password()),
    ..Default::default()
  };
  let perf = PerformanceConfig {
    default_command_timeout: Duration::from_secs(20),
    ..Default::default()
  };

  (config, perf)
}

#[cfg(not(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
)))]
fn create_redis_config(cluster: bool, resp3: bool) -> (Config, PerformanceConfig) {
  create_normal_redis_config(cluster, resp3)
}

#[cfg(all(
  feature = "enable-native-tls",
  any(feature = "enable-rustls", feature = "enable-rustls-ring")
))]
fn create_redis_config(cluster: bool, resp3: bool) -> (Config, PerformanceConfig) {
  // if both are enabled then don't use either since all the tests assume one or the other
  create_normal_redis_config(cluster, resp3)
}

#[cfg(all(
  any(feature = "enable-rustls", feature = "enable-rustls-ring"),
  not(feature = "enable-native-tls")
))]
fn create_redis_config(cluster: bool, resp3: bool) -> (Config, PerformanceConfig) {
  if !read_ci_tls_env() {
    return create_normal_redis_config(cluster, resp3);
  }

  debug!("Creating rustls test config...");
  let config = Config {
    fail_fast: read_fail_fast_env(),
    server: create_server_config(cluster),
    version: if resp3 { RespVersion::RESP3 } else { RespVersion::RESP2 },
    tls: Some(TlsConfig {
      connector: create_rustls_config(),
      hostnames: TlsHostMapping::DefaultHost,
    }),
    username: Some(read_redis_username()),
    password: Some(read_redis_password()),
    ..Default::default()
  };
  let perf = PerformanceConfig {
    default_command_timeout: Duration::from_secs(20),
    ..Default::default()
  };

  (config, perf)
}

#[cfg(all(
  feature = "enable-native-tls",
  not(any(feature = "enable-rustls", feature = "enable-rustls-ring"))
))]
fn create_redis_config(cluster: bool, resp3: bool) -> (Config, PerformanceConfig) {
  if !read_ci_tls_env() {
    return create_normal_redis_config(cluster, resp3);
  }

  debug!("Creating native-tls test config...");
  let config = Config {
    fail_fast: read_fail_fast_env(),
    server: create_server_config(cluster),
    version: if resp3 { RespVersion::RESP3 } else { RespVersion::RESP2 },
    tls: Some(TlsConfig {
      connector: create_native_tls_config(),
      hostnames: TlsHostMapping::DefaultHost,
    }),
    username: Some(read_redis_username()),
    password: Some(read_redis_password()),
    ..Default::default()
  };
  let perf = PerformanceConfig {
    default_command_timeout: Duration::from_secs(20),
    ..Default::default()
  };

  (config, perf)
}

async fn flushall_between_tests(client: &Client) -> Result<(), Error> {
  if should_flushall_between_tests() {
    client.flushall_cluster().await
  } else {
    Ok(())
  }
}

async fn check_panic(client: &Client, jh: ConnectHandle, err: Error) {
  println!("Checking panic after: {:?}", err);
  let _ = client.quit().await;
  jh.await.unwrap().unwrap();
  panic!("{:?}", err);
}

pub async fn run_sentinel<F, Fut>(func: F, resp3: bool)
where
  F: Fn(Client, Config) -> Fut,
  Fut: Future<Output = Result<(), Error>>,
{
  let policy = ReconnectPolicy::new_constant(300, RECONNECT_DELAY);
  let connection = ConnectionConfig::default();
  let config = Config {
    fail_fast: read_fail_fast_env(),
    version: if resp3 { RespVersion::RESP3 } else { RespVersion::RESP2 },
    server: ServerConfig::Sentinel {
      hosts:                                      vec![read_sentinel_server().into()],
      service_name:                               "redis-sentinel-main".into(),
      #[cfg(feature = "sentinel-auth")]
      username:                                   None,
      #[cfg(feature = "sentinel-auth")]
      password:                                   Some(read_sentinel_password()),
    },
    password: Some(read_redis_password()),
    ..Default::default()
  };
  let perf = PerformanceConfig::default();
  let client = Client::new(config.clone(), Some(perf), Some(connection), Some(policy));
  let _client = client.clone();

  let jh = client.connect();
  client.wait_for_connect().await.expect("Failed to connect client");

  if let Err(err) = flushall_between_tests(&client).await {
    check_panic(&client, jh, err).await;
  } else if let Err(err) = func(_client, config.clone()).await {
    check_panic(&client, jh, err).await;
  } else {
    let _ = client.quit().await;
    jh.await.unwrap().unwrap();
  }
}

pub async fn run_cluster<F, Fut>(func: F, resp3: bool)
where
  F: Fn(Client, Config) -> Fut,
  Fut: Future<Output = Result<(), Error>>,
{
  let (policy, cmd_attempts, fail_fast) = reconnect_settings();
  let mut connection = ConnectionConfig::default();
  let (mut config, perf) = create_redis_config(true, resp3);
  connection.max_command_attempts = cmd_attempts;
  connection.max_redirections = 10;
  connection.unresponsive = UnresponsiveConfig {
    max_timeout: Some(Duration::from_secs(10)),
    interval:    Duration::from_millis(400),
  };
  config.fail_fast = fail_fast;

  let client = Client::new(config.clone(), Some(perf), Some(connection), policy);
  let _client = client.clone();

  let jh = client.connect();
  client.wait_for_connect().await.expect("Failed to connect client");

  if let Err(err) = flushall_between_tests(&client).await {
    check_panic(&client, jh, err).await;
  } else if let Err(err) = func(_client, config.clone()).await {
    check_panic(&client, jh, err).await;
  } else {
    let _ = client.quit().await;
    jh.await.unwrap().unwrap();
  }
}

pub async fn run_centralized<F, Fut>(func: F, resp3: bool)
where
  F: Fn(Client, Config) -> Fut,
  Fut: Future<Output = Result<(), Error>>,
{
  if should_use_sentinel_config() {
    return run_sentinel(func, resp3).await;
  }

  let (policy, cmd_attempts, fail_fast) = reconnect_settings();
  let mut connection = ConnectionConfig::default();
  let (mut config, perf) = create_redis_config(false, resp3);
  connection.max_command_attempts = cmd_attempts;
  connection.unresponsive = UnresponsiveConfig {
    max_timeout: Some(Duration::from_secs(10)),
    interval:    Duration::from_millis(400),
  };
  config.fail_fast = fail_fast;

  let client = Client::new(config.clone(), Some(perf), Some(connection), policy);
  let _client = client.clone();

  let jh = client.connect();
  client.wait_for_connect().await.expect("Failed to connect client");

  if let Err(err) = flushall_between_tests(&client).await {
    check_panic(&client, jh, err).await;
  } else if let Err(err) = func(_client, config.clone()).await {
    check_panic(&client, jh, err).await;
  } else {
    let _ = client.quit().await;
    jh.await.unwrap().unwrap();
  }
}

/// Check whether the server is Valkey.
pub async fn check_valkey(client: &Client) -> bool {
  let info: String = match client.info(Some(InfoKind::Server)).await {
    Ok(val) => val,
    Err(e) => {
      warn!("Failed to check valkey server: {:?}", e);
      return false;
    },
  };

  for line in info.lines() {
    let parts: Vec<_> = line.split(":").collect();
    if parts.len() == 2 && parts[0] == "server_name" && parts[1] == "valkey" {
      return true;
    }
  }

  false
}

macro_rules! centralized_test_panic(
  ($module:tt, $name:tt) => {
    #[cfg(not(any(feature = "enable-rustls", feature = "enable-native-tls", feature = "enable-rustls-ring")))]
    mod $name {
      #[tokio::test(flavor = "multi_thread")]
      #[should_panic]
      async fn resp2() {
        if crate::integration::utils::read_ci_tls_env() {
          panic!("");
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_centralized(crate::integration::$module::$name, false).await;
      }

      #[tokio::test(flavor = "multi_thread")]
      #[should_panic]
      async fn resp3() {
        if crate::integration::utils::read_ci_tls_env() {
          panic!("");
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_centralized(crate::integration::$module::$name, true).await;
      }
    }
  }
);

macro_rules! cluster_test_panic(
  ($module:tt, $name:tt) => {
    mod $name {
      #[cfg(not(any(feature = "i-redis-stack", feature = "unix-sockets")))]
      #[tokio::test(flavor = "multi_thread")]
      #[should_panic]
      async fn resp2() {
        if crate::integration::utils::should_use_sentinel_config() {
          panic!("");
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_cluster(crate::integration::$module::$name, false).await;
      }

      #[cfg(not(any(feature = "i-redis-stack", feature = "unix-sockets")))]
      #[tokio::test(flavor = "multi_thread")]
      #[should_panic]
      async fn resp3() {
        if crate::integration::utils::should_use_sentinel_config() {
          panic!("");
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_cluster(crate::integration::$module::$name, true).await;
      }
    }
  }
);

macro_rules! centralized_test(
  ($module:tt, $name:tt) => {
    #[cfg(not(any(feature = "enable-rustls", feature = "enable-native-tls", feature = "enable-rustls-ring")))]
    mod $name {
      #[tokio::test(flavor = "multi_thread")]
      async fn resp2() {
        if crate::integration::utils::read_ci_tls_env() {
          return;
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_centralized(crate::integration::$module::$name, false).await;
      }

      #[tokio::test(flavor = "multi_thread")]
      async fn resp3() {
        if crate::integration::utils::read_ci_tls_env() {
          return;
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_centralized(crate::integration::$module::$name, true).await;
      }
    }
  }
);

macro_rules! cluster_test(
  ($module:tt, $name:tt) => {
    mod $name {
      #[cfg(not(any(feature = "i-redis-stack", feature = "unix-sockets")))]
      #[tokio::test(flavor = "multi_thread")]
      async fn resp2() {
        if crate::integration::utils::should_use_sentinel_config() {
          return;
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_cluster(crate::integration::$module::$name, false).await;
      }

      #[cfg(not(any(feature = "i-redis-stack", feature = "unix-sockets")))]
      #[tokio::test(flavor = "multi_thread")]
      async fn resp3() {
        if crate::integration::utils::should_use_sentinel_config() {
          return;
        }

        let _ = pretty_env_logger::try_init();
        crate::integration::utils::run_cluster(crate::integration::$module::$name, true).await;
      }
    }
  }
);

macro_rules! return_err(
  ($($arg:tt)*) => { {
    return Err(fred::error::Error::new(
      fred::error::ErrorKind::Unknown, format!($($arg)*)
    ));
  } }
);

macro_rules! check_redis_7 (
  ($client:ident) => {
    if $client.server_version().unwrap().major < 7 {
      return Ok(());
    }
  }
);
