use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Lint {}

impl Lint {
    pub fn run(&self) -> Result<()> {
        cargo!(["fmt", "--all", "--", "--check"]);
        cargo!(["clippy", "--all", "--", "-D", "warnings"]);

        Ok(())
    }
}
