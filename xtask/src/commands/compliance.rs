use anyhow::Result;
use xtask::*;

#[derive(Debug, clap::Parser)]
pub struct Compliance {}

impl Compliance {
    pub fn run(&self) -> Result<()> {
        // Cargo deny is triggering `git credential-manager-core get`
        // On windows CI this will hangs as it requires user input.
        // The root cause seems to be the krates step in cargo-deny but did not manage to figure it out.
        // Disabling as a temporary measure, but we must fix this soon. https://github.com/apollographql/router/issues/3237
        #[cfg(not(windows))]
        cargo!(["deny", "-L", "error", "check"]);
        Ok(())
    }
}
