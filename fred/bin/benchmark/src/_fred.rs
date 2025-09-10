use crate::{utils, Argv};
use fred::{
  clients::Pool,
  error::Error,
  prelude::*,
  types::{Builder, ClusterDiscoveryPolicy},
};
use futures::TryStreamExt;
use indicatif::ProgressBar;
use std::{
  error::Error,
  sync::{atomic::AtomicUsize, Arc},
  time::{Duration, SystemTime},
};
use tokio::task::JoinHandle;

#[cfg(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enabled-rustls-ring"
))]
use fred::types::{TlsConfig, TlsConnector, TlsHostMapping};

#[cfg(feature = "enable-rustls")]
fn default_tls_config() -> TlsConfig {
  TlsConfig {
    connector: TlsConnector::default_rustls().unwrap(),
    hostnames: TlsHostMapping::None,
  }
}

#[cfg(feature = "enable-native-tls")]
fn default_tls_config() -> TlsConfig {
  TlsConfig {
    connector: TlsConnector::default_native_tls().unwrap(),
    hostnames: TlsHostMapping::None,
  }
}

pub async fn init(argv: &Arc<Argv>) -> Result<RedisPool, RedisError> {
  let (username, password) = utils::read_auth_env();
  let config = Config {
    fail_fast: true,
    server: if argv.unix.is_some() {
      ServerConfig::Unix {
        path: argv.unix.clone().unwrap().into(),
      }
    } else if argv.cluster {
      ServerConfig::Clustered {
        hosts:  vec![Server::new(&argv.host, argv.port)],
        policy: ClusterDiscoveryPolicy::ConfigEndpoint,
      }
    } else {
      ServerConfig::new_centralized(&argv.host, argv.port)
    },
    username,
    password: argv.auth.clone().or(password),
    #[cfg(any(
      feature = "enable-native-tls",
      feature = "enable-rustls",
      feature = "enable-rustls-ring"
    ))]
    tls: default_tls_config(),
    #[cfg(any(feature = "stdout-tracing", feature = "partial-tracing", feature = "full-tracing"))]
    tracing: TracingConfig::new(argv.tracing),
    ..Default::default()
  };

  let pool = Builder::from_config(config)
    .with_connection_config(|config| {
      config.max_command_buffer_len = argv.bounded;
      config.internal_command_timeout = Duration::from_secs(5);
    })
    .set_policy(ReconnectPolicy::new_constant(0, 500))
    .build_pool(argv.pool)?;

  info!("Connecting to {}:{}...", argv.host, argv.port);
  pool.init().await?;
  info!("Connected to {}:{}.", argv.host, argv.port);
  pool.flushall_cluster().await?;

  Ok(pool)
}

fn spawn_client_task(
  bar: &Option<ProgressBar>,
  client: &Client,
  counter: &Arc<AtomicUsize>,
  argv: &Arc<Argv>,
) -> JoinHandle<()> {
  let (bar, client, counter, argv) = (bar.clone(), client.clone(), counter.clone(), argv.clone());

  tokio::spawn(async move {
    let key = utils::random_string(15);
    let mut expected = 0;

    while utils::incr_atomic(&counter) < argv.count {
      if argv.replicas {
        let _: () = client.replicas().get(&key).await.map_err(utils::crash).unwrap();
      } else {
        expected += 1;
        let actual: i64 = client.incr(&key).await.map_err(utils::crash).unwrap();

        #[cfg(feature = "assert-expected")]
        {
          if actual != expected {
            println!("Unexpected result: {} == {}", actual, expected);
            std::process::exit(1);
          }
        }
      }
      if let Some(ref bar) = bar {
        bar.inc(1);
      }
    }
    debug!("Ending client task");
  })
}

pub async fn run(argv: Arc<Argv>, counter: Arc<AtomicUsize>, bar: Option<ProgressBar>) -> Duration {
  info!("Running with fred");

  let pool = init(&argv).await.expect("Failed to init");
  let mut tasks = Vec::with_capacity(argv.tasks);

  info!("Starting commands...");
  let started = SystemTime::now();
  for _ in 0 .. argv.tasks {
    tasks.push(spawn_client_task(&bar, pool.next(), &counter, &argv));
  }
  if let Err(e) = futures::future::try_join_all(tasks).await {
    println!("Finished with error: {:?}", e);
    std::process::exit(1);
  }

  SystemTime::now()
    .duration_since(started)
    .expect("Failed to calculate duration")
}
