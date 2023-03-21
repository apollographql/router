use anyhow::Result;
use xtask::*;

const TEST_DEFAULT_ARGS: &[&str] = &["test"];

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
}

impl Test {
    pub fn run(&self) -> Result<()> {
        eprintln!("Running tests");

        let mut args = TEST_DEFAULT_ARGS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<String>>();
        if let Some(jobs) = self.jobs {
            args.push("--jobs".to_string());
            args.push(jobs.to_string());
        }

        if self.locked {
            args.push("--locked".to_string());
        }

        if self.workspace {
            args.push("--workspace".to_string());
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
