use anyhow::Result;
use xtask::*;

const TEST_DEFAULT_ARGS: &[&str] = &["test"];
const NEXTEST_DEFAULT_ARGS: &[&str] = &["nextest", "run"];

#[derive(Debug, clap::Parser)]
pub struct Test {
    /// The number of jobs to pass to cargo test via --jobs
    #[clap(long)]
    jobs: Option<usize>,

    /// The number of threads to pass to cargo test via --test-threads
    #[clap(long)]
    test_threads: Option<usize>,

    /// Pass --locked to cargo test
    #[clap(long)]
    locked: bool,

    /// Pass --workspace to cargo test
    #[clap(long)]
    workspace: bool,

    /// Pass --features to cargo test
    #[clap(long)]
    features: Option<String>,
}

impl Test {
    pub fn run(&self) -> Result<()> {
        eprintln!("Running tests");

        // Check if cargo-nextest is available.  If it is,
        // we'll use that instead of cargo test.  We will pass
        // --locked and --workspace to cargo-nextest if they are
        // desired by the configuration, but not any other arguments.
        // In the event that cargo-nextest is not available, we will
        // fall back to cargo test and pass all the arguments.
        if let Ok(_) = which::which("cargo-nextest") {
            let mut args = NEXTEST_DEFAULT_ARGS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>();

            if self.locked {
                args.push("--locked".to_string());
            }

            if self.workspace {
                args.push("--workspace".to_string());
            }

            if let Some(features) = &self.features {
                args.push("--features".to_string());
                args.push(format!("{} experimental_hyper_header_limits", features));
            } else {
                args.push("--features".to_string());
                args.push("experimental_hyper_header_limits".to_string());
            }

            cargo!(args);
            return Ok(());
        } else {
            eprintln!("cargo-nextest not found, falling back to cargo test");

            let mut args = TEST_DEFAULT_ARGS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>();

            if self.locked {
                args.push("--locked".to_string());
            }

            if self.workspace {
                args.push("--workspace".to_string());
            }

            if let Some(jobs) = self.jobs {
                args.push("--jobs".to_string());
                args.push(jobs.to_string());
            }

            args.push("--".to_string());

            if let Some(threads) = self.test_threads {
                args.push("--test-threads".to_string());
                args.push(threads.to_string());
            }
            cargo!(args);
            Ok(())
        }
    }
}
