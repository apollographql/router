use anyhow::Result;

use super::Compliance;
use super::Lint;
use super::Test;

#[derive(Debug, clap::Parser)]
pub struct Dev {
    #[clap(flatten)]
    compliance: Compliance,
    #[clap(flatten)]
    lint: Lint,
    #[clap(flatten)]
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
