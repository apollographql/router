use anyhow::Result;
use structopt::StructOpt;

use crate::target::{Target, POSSIBLE_TARGETS};
use crate::tools::{CargoRunner, FederationDemoRunner};

#[derive(Debug, StructOpt)]
pub struct Test {
    // The target to build Router for
    #[structopt(long = "target", default_value, possible_values = &POSSIBLE_TARGETS)]
    target: Target,
}

impl Test {
    pub fn run(&self, verbose: bool) -> Result<()> {
        let mut cargo_runner = CargoRunner::new(verbose)?;
        let demo = FederationDemoRunner::new(verbose)?;

        let _guard = demo.start_background()?;
        cargo_runner.test(&self.target)?;

        Ok(())
    }
}
