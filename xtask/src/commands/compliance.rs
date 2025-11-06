use anyhow::Result;
use xtask::*;

#[derive(Debug, clap::Parser, Default)]
pub struct Compliance {}

impl Compliance {
    pub fn run(&self) -> Result<()> {
        cargo!(["deny", "-L", "error", "check"]);
        Ok(())
    }
}
