use anyhow::{ensure, Result};
use structopt::StructOpt;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Lint {}

impl Lint {
    pub fn run(&self) -> Result<()> {
        Self::check_fmt()?;
        cargo!(["clippy", "--all", "--tests", "--", "-D", "warnings"]);

        Ok(())
    }

    pub fn run_local(&self) -> Result<()> {
        cargo!(["clippy", "--all", "--tests", "--", "-D", "warnings"]);

        if Self::check_fmt().is_err() {
            // cargo fmt check failed, this means there is some formatting to do
            // given this task is running locally, let's do it and let our user know
            cargo!(["fmt", "--all"]);
            eprintln!(
                "🧹 cargo fmt job is complete 🧹\n\
                Commit the changes and you should be good to go!"
            );
        };

        Ok(())
    }

    fn check_fmt() -> Result<()> {
        let cargo = which::which("cargo")?;
        let mut command = ::std::process::Command::new(cargo);
        command.args(["fmt", "--all", "--", "--check"]);

        let status = command.current_dir(&*PKG_PROJECT_ROOT).status()?;
        ensure!(status.success(), "cargo fmt check failed");
        Ok(())
    }
}
