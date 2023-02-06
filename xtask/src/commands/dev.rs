use anyhow::Result;
use structopt::StructOpt;

use super::Compliance;
use super::Lint;
use super::Test;

#[derive(Debug, StructOpt)]
pub struct Dev {
    #[structopt(flatten)]
    compliance: Compliance,
    #[structopt(flatten)]
    lint: Lint,
    #[structopt(flatten)]
    test: Test,
}

impl Dev {
    pub fn run(&self) -> Result<()> {
        eprintln!("Checking compliance...");
        self.compliance.run()?;
        eprintln!("Checking format and clippy...");
        self.lint.run_local()?;
        eprintln!("Running tests...");
        self.test.run()
    }
}
