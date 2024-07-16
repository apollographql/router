use anyhow::Result;
use xtask::*;
use xshell::*;

const PROJECT_NAME: &str = "apollo-federation-cli";

#[derive(Debug, clap::Parser)]
pub struct Flame {
    subargs: Vec<String>,
}

impl Flame {
    pub fn run(&self) -> Result<()> {
        let shell = Shell::new()?;
        match which::which("samply") {
            Err(which::Error::CannotFindBinaryPath) => {
                anyhow::bail!("samply binary not found. Try to run: cargo install samply")
            }
            Err(err) => anyhow::bail!("{err}"),
            Ok(_) => (),
        }

        cargo!(["build", "--profile", "profiling", "-p", PROJECT_NAME]);

        let subargs = &self.subargs;
        cmd!(shell, "samply record ./target/profiling/{PROJECT_NAME} {subargs...}").run()?;

        Ok(())
    }
}
