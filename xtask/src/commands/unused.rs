use std::process::Command;

use anyhow::Result;
use anyhow::ensure;
use xtask::*;

#[derive(Debug, clap::Parser)]
pub struct Unused {}

impl Unused {
    pub fn run(&self) -> Result<()> {
        let cargo = which::which("cargo-machete")?;
        let status = Command::new(&cargo)
            .current_dir(&*PKG_PROJECT_ROOT)
            .status()?;
        ensure!(status.success(), "cargo machete failed");
        Ok(())
    }
}
