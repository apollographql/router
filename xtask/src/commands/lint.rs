use std::process::Command;

use anyhow::ensure;
use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Lint {
    /// apply formatting fixes
    #[structopt(long)]
    fmt: bool,
}

const RUSTFMT_CONFIG: &[&str] = &["imports_granularity=Item", "group_imports=StdExternalCrate"];

impl Lint {
    pub fn run(&self) -> Result<()> {
        if self.fmt {
            let status = Self::fmt_command()?.status()?;
            ensure!(status.success(), "cargo fmt check failed");
            Ok(())
        } else {
            self.run_common(Self::check_fmt)
        }
    }

    pub fn run_local(&self) -> Result<()> {
        self.run_common(|| {
            if Self::check_fmt().is_err() {
                // cargo fmt check failed, this means there is some formatting to do
                // given this task is running locally, let's do it and let our user know
                let status = Self::fmt_command()?.status()?;
                ensure!(status.success(), "cargo fmt failed");
                eprintln!(
                    "🧹 cargo fmt job is complete 🧹\n\
                    Commit the changes and you should be good to go!"
                );
            };
            Ok(())
        })
    }

    fn run_common(&self, fmt: impl FnOnce() -> Result<()>) -> Result<()> {
        fmt()?;
        cargo!(["clippy", "--all", "--all-targets", "--", "-D", "warnings",]);
        cargo!(["doc", "--all", "--no-deps"], env = { "RUSTDOCFLAGS" => "-Dwarnings" });
        Ok(())
    }

    fn check_fmt() -> Result<()> {
        let status = Self::fmt_command()?.arg("--check").status()?;
        ensure!(status.success(), "cargo fmt check failed");
        Ok(())
    }

    fn fmt_command() -> Result<Command> {
        let mut command = Command::new(which::which("cargo")?);
        command.current_dir(&*PKG_PROJECT_ROOT).args([
            "fmt",
            "--all",
            "--",
            "--config",
            &RUSTFMT_CONFIG.join(","),
        ]);
        Ok(command)
    }
}
