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
  types::config::{ReplicaConfig, UnresponsiveConfig},
};
use opentelemetry::{
  global,
  sdk::{
    export::trace::stdout,
    runtime::{Runtime, Tokio},
    trace::{self, RandomIdGenerator, Sampler, TraceRuntime},
  },
};
use rand::{self, distributions::Alphanumeric, Rng};
use std::{default::Default, time::Duration};
use tokio::time::sleep;
use tracing_subscriber::{layer::SubscriberExt, Layer, Registry};

#[derive(Debug)]
struct Argv {
  pub cluster:       bool,
  pub replicas:      bool,
  pub host:          String,
  pub port:          u16,
  pub pool:          usize,
  pub interval:      u64,
  pub wait:          u64,
  pub auth:          String,
  pub tracing:       bool,
  pub sentinel:      Option<String>,
  pub sentinel_auth: Option<String>,
}

fn parse_argv() -> Argv {
  let yaml = load_yaml!("../cli.yml");
  let matches = App::from_yaml(yaml).get_matches();
  let cluster = matches.is_present("cluster");
  let replicas = matches.is_present("replicas");
  let tracing = matches.is_present("tracing");

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
    .unwrap_or(1000);
  let wait = matches
    .value_of("wait")
    .map(|v| v.parse::<u64>().expect("Invalid wait"))
    .unwrap_or(0);
  let auth = matches.value_of("auth").map(|v| v.to_owned()).unwrap_or("".into());
  let sentinel = matches.value_of("sentinel").map(|v| v.to_owned());
  let sentinel_auth = matches.value_of("sentinel-auth").map(|v| v.to_owned());

  Argv {
    cluster,
    auth,
    host,
    port,
    pool,
    interval,
    wait,
    replicas,
    tracing,
    sentinel,
    sentinel_auth,
  }
}

#[cfg(all(
  not(feature = "partial-tracing"),
  not(feature = "stdout-tracing"),
  not(feature = "full-tracing")
))]
pub fn setup_tracing(enable: bool) {}

#[cfg(feature = "stdout-tracing")]
pub fn setup_tracing(enable: bool) {
  if enable {
    info!("Starting stdout tracing...");
    let layer = tracing_subscriber::fmt::layer()
      .with_writer(std::io::stdout)
      .with_ansi(false)
      .event_format(tracing_subscriber::fmt::format().pretty())
      .with_thread_names(true)
      .with_level(true)
      .with_line_number(true)
      .with_filter(tracing_subscriber::filter::LevelFilter::TRACE);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global tracing subscriber");
  }
}

#[cfg(any(feature = "partial-tracing", feature = "full-tracing"))]
pub fn setup_tracing(enable: bool) {
  let sampler = if enable {
    info!("Starting tracing...");
    Sampler::AlwaysOn
  } else {
    Sampler::AlwaysOff
  };

  global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
  let jaeger_install = opentelemetry_jaeger::new_agent_pipeline()
    .with_service_name("fred-inf-loop")
    .with_endpoint("jaeger:6831")
    .with_trace_config(
      trace::config()
        .with_sampler(sampler)
        .with_id_generator(RandomIdGenerator::default())
        .with_max_attributes_per_span(32),
    )
    .install_simple();

  let tracer = match jaeger_install {
    Ok(t) => t,
    Err(e) => panic!("Fatal error initializing tracing: {:?}", e),
  };

  let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
  let subscriber = Registry::default().with(telemetry);
  tracing::subscriber::set_global_default(subscriber).expect("Failed to set global tracing subscriber");

  info!("Initialized opentelemetry-jaeger pipeline.");
}

#[tokio::main]
async fn main() -> Result<(), Error> {
  pretty_env_logger::init_timed();
  let argv = parse_argv();
  info!("Running with configuration: {:?}", argv);
  setup_tracing(argv.tracing);

  let config = Config {
    #[cfg(any(feature = "partial-tracing", feature = "stdout-tracing", feature = "full-tracing"))]
    tracing: TracingConfig {
      enabled: argv.tracing,
      ..Default::default()
    },
    server: if argv.cluster {
      ServerConfig::new_clustered(vec![(&argv.host, argv.port)])
    } else {
      if let Some(sentinel) = argv.sentinel.as_ref() {
        ServerConfig::Sentinel {
          service_name: sentinel.to_string(),
          hosts:        vec![Server::new(&argv.host, argv.port)],
          password:     argv.sentinel_auth.clone(),
          username:     None,
        }
      } else {
        ServerConfig::new_centralized(&argv.host, argv.port)
      }
    },
    password: if argv.auth.is_empty() {
      None
    } else {
      Some(argv.auth.clone())
    },
    ..Default::default()
  };
  let pool = Builder::from_config(config)
    .with_connection_config(|config| {
      config.max_command_attempts = 3;
      // config.unresponsive = UnresponsiveConfig {
      //  interval:    Duration::from_secs(1),
      //  max_timeout: Some(Duration::from_secs(5)),
      //};
      config.connection_timeout = Duration::from_secs(3);
      config.internal_command_timeout = Duration::from_secs(2);
      // config.cluster_cache_update_delay = Duration::from_secs(20);
      if argv.replicas {
        config.replica = ReplicaConfig {
          lazy_connections: true,
          primary_fallback: true,
          ..Default::default()
        };
      }
    })
    .with_performance_config(|config| {
      config.default_command_timeout = Duration::from_secs(60 * 5);
    })
    .set_policy(ReconnectPolicy::new_linear(0, 5000, 100))
    .build_pool(argv.pool)
    .expect("Failed to create pool");

  info!("Connecting to {}:{}...", argv.host, argv.port);
  pool.init().await?;
  info!("Connected to {}:{}", argv.host, argv.port);
  pool.flushall_cluster().await?;

  if argv.wait > 0 {
    info!("Waiting for {} ms", argv.wait);
    sleep(Duration::from_millis(argv.wait)).await;
  }

  tokio::spawn(async move {
    tokio::signal::ctrl_c().await;
    std::process::exit(0);
  });
  loop {
    if argv.replicas {
      let _: Option<Bytes> = pool.replicas().get("foo").await.expect("Failed to GET");
    } else {
      let _: i64 = pool.incr("foo").await.expect("Failed to INCR");
    }
    sleep(Duration::from_millis(argv.interval)).await;
  }
}
