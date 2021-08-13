use anyhow::{Context, Result};
use structopt::StructOpt;

use crate::commands::version::RouterVersion;
use crate::target::{Target, POSSIBLE_TARGETS};
use crate::tools::{CargoRunner, StripRunner};

#[derive(Debug, StructOpt)]
pub struct Dist {
    /// The target to build Router for
    #[structopt(long = "target", default_value, possible_values = &POSSIBLE_TARGETS)]
    target: Target,

    // The version to check out and compile, otherwise install a local build
    #[structopt(long)]
    version: Option<RouterVersion>,
}

impl Dist {
    pub fn run(&self, verbose: bool) -> Result<()> {
        let mut cargo_runner = CargoRunner::new(verbose)?;
        let binary_path = cargo_runner
            .build(&self.target, true, self.version.as_ref())
            .with_context(|| "Could not build Router.")?;

        if !cfg!(windows) {
            let strip_runner = StripRunner::new(binary_path, verbose)?;
            strip_runner
                .run()
                .with_context(|| "Could not strip symbols from Router's binary")?;
        }

        Ok(())
    }
}
