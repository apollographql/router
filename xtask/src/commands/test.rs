use anyhow::Result;
use structopt::StructOpt;

use crate::target::{Target, POSSIBLE_TARGETS};
use crate::tools::{CargoRunner, FederationDemoRunner};
use crate::utils;

#[derive(Debug, StructOpt)]
pub struct Test {
    // The target to build Router for
    #[structopt(long = "target", default_value, possible_values = &POSSIBLE_TARGETS)]
    target: Target,

    /// Do not start federation demo.
    #[structopt(long)]
    no_demo: bool,
}

impl Test {
    pub fn run(&self, verbose: bool) -> Result<()> {
        let mut cargo_runner = CargoRunner::new(verbose)?;

        // NOTE: it worked nicely on GitHub Actions but it hangs on CircleCI on Windows
        let _guard: Box<dyn std::any::Any> = if !std::env::var("CIRCLECI")
            .ok()
            .unwrap_or_default()
            .is_empty()
            && cfg!(windows)
        {
            utils::info("Not running federation-demo because it makes the step hang on Circle CI.");
            Box::new(())
        } else if self.no_demo {
            utils::info("Not running federation-demo as requested.");
            Box::new(())
        } else {
            let demo = FederationDemoRunner::new(verbose)?;
            let guard = demo.start_background()?;
            Box::new((demo, guard))
        };

        cargo_runner.test(&self.target)?;

        Ok(())
    }
}
