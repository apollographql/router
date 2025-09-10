#![allow(warnings)]

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[macro_use]
extern crate clap;

#[macro_use]
extern crate log;
extern crate pretty_env_logger;

#[cfg(any(feature = "partial-tracing", feature = "full-tracing", feature = "stdout-tracing"))]
use fred::types::TracingConfig;

use clap::App;
use indicatif::ProgressBar;
#[cfg(feature = "tracing-deps")]
use opentelemetry::{
  global,
  sdk::{
    export::trace::stdout,
    runtime::{Runtime, Tokio},
    trace::{self, RandomIdGenerator, Sampler, TraceRuntime},
  },
};
#[cfg(feature = "tracing-deps")]
use opentelemetry_jaeger::JaegerTraceRuntime;
use std::{
  default::Default,
  env,
  sync::{atomic::AtomicUsize, Arc},
  thread::{self},
  time::{Duration, SystemTime},
};
use tokio::{runtime::Builder, task::JoinHandle, time::Instant};
#[cfg(feature = "tracing-deps")]
use tracing_subscriber::{layer::SubscriberExt, Layer, Registry};

static DEFAULT_COMMAND_COUNT: usize = 10_000;
static DEFAULT_CONCURRENCY: usize = 10;
static DEFAULT_HOST: &'static str = "127.0.0.1";
static DEFAULT_PORT: u16 = 6379;

mod utils;

#[cfg(all(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
))]
compile_error!("Cannot use both TLS feature flags.");

#[cfg(not(feature = "redis-rs"))]
mod _fred;
#[cfg(feature = "redis-rs")]
mod _redis;
#[cfg(not(feature = "redis-rs"))]
use _fred::run as run_benchmark;
#[cfg(feature = "redis-rs")]
use _redis::run as run_benchmark;

// TODO update clap
#[derive(Debug)]
struct Argv {
  pub cluster:  bool,
  pub replicas: bool,
  pub bounded:  usize,
  pub tracing:  bool,
  pub count:    usize,
  pub tasks:    usize,
  pub unix:     Option<String>,
  pub host:     String,
  pub port:     u16,
  pub pool:     usize,
  pub quiet:    bool,
  pub auth:     Option<String>,
}

fn parse_argv() -> Arc<Argv> {
  let yaml = load_yaml!("../cli.yml");
  let matches = App::from_yaml(yaml).get_matches();
  let tracing = matches.is_present("tracing");
  let mut cluster = matches.is_present("cluster");
  let replicas = matches.is_present("replicas");
  let quiet = matches.is_present("quiet");

  if replicas {
    cluster = true;
  }

  let count = matches
    .value_of("count")
    .map(|v| {
      v.parse::<usize>().unwrap_or_else(|_| {
        panic!("Invalid command count: {}.", v);
      })
    })
    .unwrap_or(DEFAULT_COMMAND_COUNT);
  let tasks = matches
    .value_of("concurrency")
    .map(|v| {
      v.parse::<usize>().unwrap_or_else(|_| {
        panic!("Invalid concurrency: {}.", v);
      })
    })
    .unwrap_or(DEFAULT_CONCURRENCY);
  let host = matches
    .value_of("host")
    .map(|v| v.to_owned())
    .unwrap_or("127.0.0.1".into());
  let port = matches
    .value_of("port")
    .map(|v| v.parse::<u16>().expect("Invalid port"))
    .unwrap_or(DEFAULT_PORT);
  let unix = matches.value_of("unix").map(|v| v.to_owned());
  let pool = matches
    .value_of("pool")
    .map(|v| v.parse::<usize>().expect("Invalid pool"))
    .unwrap_or(1);
  let bounded = matches
    .value_of("bounded")
    .map(|v| v.parse::<usize>().expect("Invalid bounded value"))
    .unwrap_or(0);
  let auth = matches.value_of("auth").map(|v| v.to_owned());

  Arc::new(Argv {
    cluster,
    quiet,
    unix,
    tracing,
    count,
    tasks,
    host,
    port,
    bounded,
    pool,
    replicas,
    auth,
  })
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
    .with_service_name("fred-benchmark")
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

fn main() {
  #[cfg(feature = "dhat-heap")]
  let _profiler = dhat::Profiler::new_heap();

  pretty_env_logger::init();
  #[cfg(feature = "console")]
  console_subscriber::init();

  let argv = parse_argv();
  info!("Running with configuration: {:?}", argv);
  thread::spawn(move || {
    let sch = Builder::new_multi_thread().enable_all().build().unwrap();
    sch.block_on(async move {
      tokio::spawn(async move {
        #[cfg(feature = "metrics")]
        let monitor = tokio_metrics::RuntimeMonitor::new(&tokio::runtime::Handle::current());

        setup_tracing(argv.tracing);
        let counter = Arc::new(AtomicUsize::new(0));
        let bar = if argv.quiet {
          None
        } else {
          Some(ProgressBar::new(argv.count as u64))
        };

        #[cfg(feature = "metrics")]
        let monitor_jh = tokio::spawn(async move {
          for interval in monitor.intervals() {
            println!("{:?}", interval);
            tokio::time::sleep(Duration::from_secs(2)).await;
          }
        });

        let duration = run_benchmark(argv.clone(), counter, bar.clone()).await;
        let duration_sec = duration.as_secs() as f64 + (duration.subsec_millis() as f64 / 1000.0);
        if let Some(bar) = bar {
          bar.finish();
        }

        #[cfg(feature = "metrics")]
        monitor_jh.abort();

        if argv.quiet {
          println!("{}", (argv.count as f64 / duration_sec) as u64);
        } else {
          println!(
            "Performed {} operations in: {:?}. Throughput: {} req/sec",
            argv.count,
            duration,
            (argv.count as f64 / duration_sec) as u64
          );
        }
        #[cfg(feature = "tracing-deps")]
        global::shutdown_tracer_provider();
      })
      .await;
    });
  })
  .join()
  .unwrap();
}
