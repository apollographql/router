use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

const TEST_DEFAULT_ARGS: &[&str] = &["test"];

#[derive(Debug, StructOpt)]
pub struct Test {
    /// The number of jobs to pass to cargo test via --jobs
    #[structopt(long)]
    jobs: Option<usize>,

    /// The number of threads to pass to cargo test via --test-threads
    #[structopt(long)]
    test_threads: Option<usize>,

    /// Pass --locked to cargo test
    #[structopt(long)]
    locked: bool,

    /// Pass --workspace to cargo test
    #[structopt(long)]
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
