#[macro_use]
extern crate clap;
extern crate fred;
extern crate futures;
extern crate tokio;

#[macro_use]
extern crate log;
extern crate pretty_env_logger;

use clap::App;
use fred::{
  bytes::Bytes,
  prelude::*,
  types::{ReconnectError, ReplicaConfig, UnresponsiveConfig},
};
use rand::{self, distributions::Alphanumeric, Rng};
use std::{
  default::Default,
  sync::Arc,
  time::{Duration, Instant},
};
use tokio::{task::JoinSet, time::sleep};

#[derive(Debug)]
struct Argv {
  pub host:        String,
  pub port:        u16,
  pub pool:        usize,
  pub interval:    u64,
  pub concurrency: u64,
  pub auth:        String,
  pub wait:        bool,
}

fn parse_argv() -> Arc<Argv> {
  let yaml = load_yaml!("../cli.yml");
  let matches = App::from_yaml(yaml).get_matches();
  let wait = matches.is_present("wait");

  let host = matches
    .value_of("host")
    .map(|v| v.to_owned())
    .unwrap_or("127.0.0.1".into());
  let port = matches
    .value_of("port")
    .map(|v| v.parse::<u16>().expect("Invalid port"))
    .unwrap_or(6379);
  let pool = matches
    .value_of("pool")
    .map(|v| v.parse::<usize>().expect("Invalid pool"))
    .unwrap_or(1);
  let interval = matches
    .value_of("interval")
    .map(|v| v.parse::<u64>().expect("Invalid interval"))
    .unwrap_or(500);
  let concurrency = matches
    .value_of("concurrency")
    .map(|v| v.parse::<u64>().expect("Invalid concurrency"))
    .unwrap_or(500);
  let auth = matches.value_of("auth").map(|v| v.to_owned()).unwrap_or("".into());

  Arc::new(Argv {
    auth,
    host,
    port,
    pool,
    interval,
    concurrency,
    wait,
  })
}

#[tokio::main]
async fn main() -> Result<(), RedisError> {
  pretty_env_logger::init_timed();
  let argv = parse_argv();
  info!("Running with configuration: {:?}", argv);

  let config = RedisConfig {
    server: ServerConfig::new_clustered(vec![(&argv.host, argv.port)]),
    password: if argv.auth.is_empty() {
      None
    } else {
      Some(argv.auth.clone())
    },
    ..Default::default()
  };
  let pool = Builder::from_config(config)
    .with_connection_config(|config| {
      config.max_command_attempts = 10;
      config.unresponsive = UnresponsiveConfig {
        interval:    Duration::from_millis(500),
        max_timeout: Some(Duration::from_secs(3)),
      };
      config.connection_timeout = Duration::from_secs(3);
      config.internal_command_timeout = Duration::from_secs(1);
      config.cluster_cache_update_delay = Duration::from_millis(100);
      // config.cluster_cache_update_delay = Duration::from_secs(20);
      config.replica = ReplicaConfig {
        connection_error_count: 1,
        ..Default::default()
      };
      config.reconnect_errors = vec![
        ReconnectError::ClusterDown,
        ReconnectError::Loading,
        ReconnectError::MasterDown,
        ReconnectError::ReadOnly,
        ReconnectError::Misconf,
        ReconnectError::Busy,
        ReconnectError::NoReplicas,
      ];
    })
    .with_performance_config(|config| {
      config.default_command_timeout = Duration::from_secs(60);
    })
    .set_policy(ReconnectPolicy::new_constant(0, 50))
    .build_pool(argv.pool)
    .expect("Failed to create pool");

  info!("Connecting to {}:{}...", argv.host, argv.port);
  pool.init().await?;
  info!("Connected to {}:{}.", argv.host, argv.port);
  pool.flushall_cluster().await?;

  tokio::spawn(async move {
    tokio::signal::ctrl_c().await;
    std::process::exit(0);
  });

  let mut interval = tokio::time::interval(Duration::from_millis(argv.interval));
  loop {
    interval.tick().await;
    test(&argv, &pool).await.unwrap();
  }
}

async fn test(argv: &Arc<Argv>, pool: &RedisPool) -> Result<(), RedisError> {
  let start = Instant::now();
  let mut tesks = (0 .. argv.concurrency).fold(JoinSet::new(), |mut tasks, _| {
    let pool = pool.clone();
    let should_wait = argv.wait;
    tasks.spawn(async move {
      if should_wait {
        let client = pool.next();
        client.set("foo", 12345u64, None, None, false).await?;
        // TODO need a `hash_slot` field on `Options` for `WAIT` to be useful in this context.
        client.wait(1, 10).await?;
        client.replicas().get::<Option<u64>, _>("foo").await
      } else {
        pool.set("foo", 12345u64, None, None, false).await?;
        pool.replicas().get::<Option<u64>, _>("foo").await
      }
    });
    tasks
  });

  let mut success = 0;
  let mut errors = 0;
  while let Some(res) = tesks.join_next().await {
    match res? {
      Ok(value) => {
        if value != Some(12345u64) {
          debug!("Redis error: empty value by key!");
          errors += 1;
        } else {
          success += 1;
        }
      },
      Err(e) => {
        debug!("Redis error: {e}");
        errors += 1;
      },
    }
  }

  let elapsed = start.elapsed();
  if errors > 0 {
    error!("-[ERR] {success}/{errors} Took {elapsed:?}");
  } else {
    info!("+[OK] {success}/{errors} Took {elapsed:?}");
  }
  Ok(())
}
