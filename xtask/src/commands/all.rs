use anyhow::Result;
use structopt::StructOpt;

use super::Compliance;
use super::Licenses;
use super::Lint;
use super::Test;

#[derive(Debug, StructOpt)]
pub struct All {
    #[structopt(flatten)]
    compliance: Compliance,
    #[structopt(flatten)]
    licenses: Licenses,
    #[structopt(flatten)]
    lint: Lint,
    #[structopt(flatten)]
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
