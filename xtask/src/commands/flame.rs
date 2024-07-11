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
        std::process::Command::new(samply)
            .arg("record")
            .arg(format!("./target/profiling/{PROJECT_NAME}"))
            .args(&self.subargs)
            .env("CARGO_PROFILE_RELEASE_DEBUG", "true")
            .output()?;

        Ok(())
    }
}
