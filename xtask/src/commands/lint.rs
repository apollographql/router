use anyhow::Result;
use structopt::StructOpt;

use crate::tools::CargoRunner;

#[derive(Debug, StructOpt)]
pub struct Lint {}

impl Lint {
    pub fn run(&self, verbose: bool) -> Result<()> {
        let mut cargo_runner = CargoRunner::new(verbose)?;
        cargo_runner.lint()?;
        Ok(())
    }
}
