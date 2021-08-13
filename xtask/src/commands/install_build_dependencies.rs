use anyhow::Result;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct InstallBuildDependencies {}

impl InstallBuildDependencies {
    #[cfg(target_os = "macos")]
    pub fn run(&self) -> Result<()> {
        use anyhow::{ensure, Context};
        use std::process::Command;

        const DEPENDENCIES: &[&str] = &["openssl@1.1"];

        for package in DEPENDENCIES {
            crate::info!("Installing {} via brew...", package);
            ensure!(
                Command::new("brew")
                    .arg("install")
                    .arg(package)
                    .status()
                    .context("could not start command brew")?
                    .success(),
                "installation of {} failed",
                package,
            );
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn run(&self) -> Result<()> {
        Ok(())
    }
}
