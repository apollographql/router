use anyhow::{ensure, Result};
use std::{thread::sleep, time::Duration};
use structopt::StructOpt;
use xtask::*;

const FEATURE_SETS: &[&[&str]] = &[
    &["otlp-tonic", "tls"],
    &["otlp-tonic"],
    &["otlp-http"],
    &["otlp-grpcio"],
    &[],
];

#[derive(Debug, StructOpt)]
pub struct Test {
    /// Do not start federation demo (deprecated, this is the default now).
    #[structopt(long, conflicts_with = "with-demo")]
    no_demo: bool,

    /// Do start the federation demo (without docker).
    #[structopt(long, conflicts_with = "no-demo")]
    with_demo: bool,
}

impl Test {
    pub fn run(&self) -> Result<()> {
        ensure!(
            !(self.no_demo && self.with_demo),
            "--no-demo and --with-demo are mutually exclusive",
        );

        // start building tests, while the subservices are spinning up
        eprintln!("Starting to build tests...");
        let _build = std::process::Command::new(which::which("cargo")?)
            .args([
                "test",
                "--no-run",
                "--locked",
                "-p",
                "apollo-router",
                "-p",
                "apollo-router-core",
                "--no-default-features",
                "--features",
                "otlp-tonic, tls",
            ])
            .spawn()?;

        // NOTE: it worked nicely on GitHub Actions but it hangs on CircleCI on Windows
        let _guard: Box<dyn std::any::Any> = if !std::env::var("CIRCLECI")
            .ok()
            .unwrap_or_default()
            .is_empty()
            && cfg!(windows)
        {
            eprintln!("Not running federation-demo because it makes the step hang on Circle CI.");
            Box::new(())
        } else if self.no_demo {
            eprintln!("Flag --no-demo is the default now. Not running federation-demo.");
            Box::new(())
        } else if !self.with_demo {
            eprintln!("Not running federation-demo.");
            Box::new(())
        } else {
            let demo = FederationDemoRunner::new()?;
            let guard = demo.start_background()?;
            Box::new((demo, guard))
        };

        eprintln!("Waiting for service to be ready...");
        loop {
            match reqwest::blocking::get("http://localhost:4100/graphql") {
                Ok(_) => break,
                Err(err) => eprintln!("{}", err),
            }
            sleep(Duration::from_secs(2));
        }

        for features in FEATURE_SETS {
            if cfg!(windows) && features.contains(&"otlp-grpcio") {
                // TODO: I couldn't make it build on Windows but it is supposed to build.
                continue;
            }

            eprintln!("Running tests with features: {}", features.join(", "));
            cargo!(
                ["test", "--workspace", "--locked", "--no-default-features"],
                features.iter().flat_map(|feature| ["--features", feature]),
            );
        }

        Ok(())
    }
}
