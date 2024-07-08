use anyhow::Result;
use xtask::*;

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
        let mut args = vec![];

        if self.locked {
            args.push("--locked".to_string());
        }

        if self.workspace {
            args.push("--workspace".to_string());
        }

        if let Some(features) = &self.features {
            args.push("--features".to_string());
            args.push(features.to_owned());
        }

        // Check if cargo-nextest is available.  If it is,
        // we'll use that instead of cargo test.  We will pass
        // --locked and --workspace to cargo-nextest if they are
        // desired by the configuration, but not any other arguments.
        // In the event that cargo-nextest is not available, we will
        // fall back to cargo test and pass all the arguments.
        if which::which("cargo-nextest").is_ok() {
            // Check if cargo-llvm-cov is available.  If it is, add the
            // cargo-llvm-cov command as an argument before the first argument.
            if which::which("cargo-llvm-cov").is_ok() {
                println!("cargo-llvm-cov found, using it IN ADDITION to cargo-nextest");
                let mut new_args = vec!["llvm-cov".to_string(), "nextest".to_string()];
                new_args.extend(args);
                cargo!(new_args);
            } else {
                let mut new_args = vec!["nextest".to_string(), "run".to_string()];
                new_args.extend(args);
                cargo!(new_args);
            }
        } else {
            eprintln!("cargo-nextest not found, falling back to cargo test");
            if let Some(jobs) = self.jobs {
                args.push("--jobs".to_string());
                args.push(jobs.to_string());
            }

            args.push("--".to_string());

            if let Some(threads) = self.test_threads {
                args.push("--test-threads".to_string());
                args.push(threads.to_string());
            }

            let mut new_args = vec!["test".to_string()];
            new_args.extend(args);
            cargo!(new_args);
        }
        Ok(())
    }
}
