use anyhow::Result;
use clap::Parser;

use super::Compliance;
use super::Licenses;
use super::Lint;
use super::Test;

#[derive(Debug, Parser)]
pub struct All {
    #[command(flatten)]
    compliance: Compliance,
    #[command(flatten)]
    licenses: Licenses,
    #[command(flatten)]
    lint: Lint,
    #[command(flatten)]
    test: Test,
}

impl All {
    pub fn run(&self) -> Result<()> {
        eprintln!("Checking licenses...");
        self.licenses.run()?;
        eprintln!("Checking compliance...");
        self.compliance.run()?;
        eprintln!("Checking format and clippy...");
        self.lint.run_local()?;
        eprintln!("Running tests...");
        self.test.run()
    }
}
