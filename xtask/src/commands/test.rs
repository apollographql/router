use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

const FEATURE_SETS: &[&[&str]] = &[
    &[],
    &["otlp-http"],
    &["otlp-tonic"],
    &["otlp-tonic", "tls"],
    &["otlp-grpcio"],
];

#[derive(Debug, StructOpt)]
pub struct Test {
    /// Do not start federation demo.
    #[structopt(long)]
    no_demo: bool,
}

impl Test {
    pub fn run(&self) -> Result<()> {
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
            eprintln!("Not running federation-demo as requested.");
            Box::new(())
        } else {
            let demo = FederationDemoRunner::new()?;
            let guard = demo.start_background()?;
            Box::new((demo, guard))
        };

        for features in FEATURE_SETS {
            if cfg!(windows) && features.contains(&"otlp-grpcio") {
                // TODO: I couldn't make it build on Windows but it is supposed to build.
                continue;
            }

            eprintln!("Running tests with features: {}", features.join(", "));
            cargo!(
                ["test", "--workspace", "--locked", "--no-default-features"],
                ["--features", "apollo-router-core/post-processing"],
                features.iter().flat_map(|feature| ["--features", feature]),
            );
        }

        Ok(())
    }
}
