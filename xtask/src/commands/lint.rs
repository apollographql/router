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
            let [mut command_1, mut command_2] = Self::fmt_commands()?;
            let status = command_1.status()?;
            ensure!(status.success(), "cargo fmt failed");
            let status = command_2.status()?;
            ensure!(status.success(), "cargo fmt failed");
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
                let [mut command_1, mut command_2] = Self::fmt_commands()?;
                let status = command_1.status()?;
                ensure!(status.success(), "cargo fmt failed");
                let status = command_2.status()?;
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
        let [mut command_1, mut command_2] = Self::fmt_commands()?;
        let status = command_1.arg("--check").status()?;
        ensure!(status.success(), "cargo fmt check failed");
        let status = command_2.arg("--check").status()?;
        ensure!(status.success(), "cargo fmt check failed");
        Ok(())
    }

    fn fmt_commands() -> Result<[Command; 2]> {
        let cargo = which::which("cargo")?;
        let args = ["fmt", "--all", "--", "--config", &RUSTFMT_CONFIG.join(",")];
        let mut command_1 = Command::new(&cargo);
        let mut command_2 = Command::new(&cargo);
        let root = &*PKG_PROJECT_ROOT;
        command_1.args(args).current_dir(root);
        command_2.args(args).current_dir(root.join("xtask"));
        Ok([command_1, command_2])
    }
}
