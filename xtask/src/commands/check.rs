use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Check {}

impl Check {
    pub fn run(&self) -> Result<()> {
        cargo!(["deny", "-L", "error", "check"]);

        Ok(())
    }
}
