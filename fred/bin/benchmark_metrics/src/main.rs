#[macro_use]
extern crate log;
extern crate pretty_env_logger;

use clap::{load_yaml, App};
use csv::WriterBuilder;
use indicatif::ProgressBar;
use std::time::Duration;
use subprocess::{Popen, PopenConfig, Redirection};

// TODO update clap
struct Argv {
  pub count:             u64,
  pub pipeline:          bool,
  pub pool_range:        (u32, u32),
  pub pool_step:         u32,
  pub concurrency_range: (u32, u32),
  pub concurrency_step:  u32,
  pub host:              String,
  pub port:              u16,
  pub cluster:           bool,
}

fn parse_argv() -> Argv {
  let yaml = load_yaml!("../cli.yml");
  let matches = App::from_yaml(yaml).get_matches();
  let cluster = matches.is_present("cluster");

  let count = matches
    .value_of("count")
    .map(|v| {
      v.parse::<u64>().unwrap_or_else(|_| {
        panic!("Invalid command count: {}.", v);
      })
    })
    .expect("Invalid count");
  let concurrency_range = matches
    .value_of("concurrency")
    .map(|v| {
      let parts: Vec<&str> = v.split("-").collect();
      (
        parts[0].parse::<u32>().expect("Invalid concurrency range"),
        parts[1].parse::<u32>().expect("Invalid concurrency range"),
      )
    })
    .expect("Invalid concurrency");
  let concurrency_step = matches
    .value_of("concurrency-step")
    .map(|v| v.parse::<u32>().expect("Invalid concurrency."))
    .expect("Invalid concurrency step");
  let pool_range = matches
    .value_of("pool")
    .map(|v| {
      let parts: Vec<&str> = v.split("-").collect();
      (
        parts[0].parse::<u32>().expect("Invalid pool range"),
        parts[1].parse::<u32>().expect("Invalid pool range"),
      )
    })
    .expect("Invalid pool range");
  let pool_step = matches
    .value_of("pool-step")
    .map(|v| v.parse::<u32>().expect("Invalid pool."))
    .expect("Invalid pool range");
  let host = matches
    .value_of("host")
    .map(|v| v.to_owned())
    .unwrap_or("127.0.0.1".into());
  let port = matches
    .value_of("port")
    .map(|v| v.parse::<u16>().expect("Invalid port"))
    .unwrap_or(6379);
  let pipeline = matches.subcommand_matches("pipeline").is_some();

  Argv {
    pool_range,
    pool_step,
    concurrency_range,
    concurrency_step,
    cluster,
    count,
    host,
    port,
    pipeline,
  }
}

struct Metrics {
  pub concurrency: u32,
  pub pool:        u32,
  pub throughput:  f64,
}

fn run_command(argv: &Argv, bar: &ProgressBar, concurrency: u32, pool: u32) -> Metrics {
  let mut parts = vec![
    "cargo".into(),
    "run".into(),
    "--release".into(),
    "--manifest-path".into(),
    // if not using docker
    //"../benchmark/Cargo.toml".into(),
    // if using docker
    "/benchmark/Cargo.toml".into(),
    "--".into(),
    "-q".into(),
    "-h".into(),
    argv.host.clone(),
    "-p".into(),
    argv.port.to_string(),
    "-n".into(),
    argv.count.to_string(),
  ];
  if argv.cluster {
    parts.push("--cluster".into());
  }
  parts.extend(vec!["-c".into(), concurrency.to_string()]);
  parts.extend(vec!["-P".into(), pool.to_string()]);
  parts.push(if argv.pipeline {
    "pipeline".into()
  } else {
    "no-pipeline".into()
  });
  debug!("Running command: {:?}", parts);

  let mut process = Popen::create(&parts, PopenConfig {
    stdout: Redirection::Pipe,
    stderr: Redirection::Pipe,
    ..Default::default()
  })
  .expect("Failed to spawn process");
  if process
    .wait_timeout(Duration::from_secs(120))
    .expect("Failed to wait on subprocess.")
    .is_none()
  {
    panic!("Timeout running with pool: {}, concurrency: {}", pool, concurrency);
  }
  let (throughput, stderr) = process.communicate(None).expect("Failed to read process output");
  debug!("Recv output: {:?}, stderr: {:?}", throughput, stderr);
  bar.inc(1);

  let throughput = throughput
    .expect("Missing output")
    .trim()
    .parse::<f64>()
    .expect("Failed to parse output");
  Metrics {
    concurrency,
    pool,
    throughput,
  }
}

fn print_output(data: Vec<Metrics>) {
  let mut wtr = WriterBuilder::new().from_writer(vec![]);
  let _ = wtr.write_record(&["concurrency", "pool", "throughput"]);

  for metrics in data.into_iter() {
    let _ = wtr.write_record(&[
      metrics.concurrency.to_string(),
      metrics.pool.to_string(),
      metrics.throughput.to_string(),
    ]);
  }

  println!(
    "{}",
    String::from_utf8(wtr.into_inner().expect("Failed to write CSV.")).expect("Failed to convert CSV to string.")
  );
}

fn main() {
  pretty_env_logger::init();
  let argv = parse_argv();
  let concurrency_runs = (argv.concurrency_range.1 - argv.concurrency_range.0 + 1) / argv.concurrency_step;
  let pool_runs = (argv.pool_range.1 - argv.pool_range.0 + 1) / argv.pool_step;
  trace!("Concurrency runs: {}, pool runs: {}", concurrency_runs, pool_runs);
  let bar = ProgressBar::new((concurrency_runs * pool_runs) as u64);

  let mut output = Vec::with_capacity((concurrency_runs * pool_runs) as usize);

  let mut pool = argv.pool_range.0;
  while pool <= argv.pool_range.1 {
    let mut concurrency = argv.concurrency_range.0;

    while concurrency <= argv.concurrency_range.1 {
      debug!("Running with concurrency: {}, pool: {}", concurrency, pool);

      if concurrency < pool {
        bar.inc(1);
        concurrency += argv.concurrency_step;
        continue;
      }

      let metrics = run_command(&argv, &bar, concurrency, pool);
      output.push(metrics);
      concurrency += argv.concurrency_step;
    }

    pool += argv.pool_step;
  }
  bar.finish();

  print_output(output);
}
