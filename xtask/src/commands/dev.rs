use anyhow::Result;

use super::Compliance;
use super::Lint;
use super::Test;
use super::Unused;

#[derive(Debug, clap::Parser)]
pub struct Dev {
    #[clap(flatten)]
    compliance: Compliance,
    #[clap(flatten)]
    lint: Lint,
    #[clap(flatten)]
    test: Test,
    #[clap(flatten)]
    unused: Unused,
}

impl Dev {
    pub fn run(&self) -> Result<()> {
        eprintln!("Checking compliance...");
        self.compliance.run()?;
        eprintln!("Checking format and clippy...");
        self.lint.run_local()?;
        eprintln!("Running tests...");
        self.test.run()?;
        eprintln!("Checking dependencies...");
        self.unused.run()?;

        Ok(())
    }
}
