use std::process::Command;
use std::process::Stdio;

use anyhow::Result;
use xtask::*;

const PROJECT_NAME: &str = "apollo-federation-cli";

#[derive(Debug, clap::Parser)]
pub struct Flame {
    subargs: Vec<String>,
}

impl Flame {
    pub fn run(&self) -> Result<()> {
        let samply = which::which("samply").map_err(|err| match err {
            which::Error::CannotFindBinaryPath => {
                anyhow::anyhow!("samply binary not found. Try to run: cargo install samply")
            }
            err => anyhow::anyhow!("{err}"),
        })?;

        cargo!(["build", "--profile", "profiling", "-p", PROJECT_NAME]);
        let status = Command::new(samply)
            .arg("record")
            .arg(format!("./target/profiling/{PROJECT_NAME}"))
            .args(&self.subargs)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()?;
        anyhow::ensure!(status.success(), "samply exited with {status}");

        Ok(())
    }
}
