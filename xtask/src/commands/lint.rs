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

    /// skip xtask subproject
    #[structopt(long)]
    skip_xtask: bool,
}

const RUSTFMT_CONFIG: &[&str] = &["imports_granularity=Item", "group_imports=StdExternalCrate"];

impl Lint {
    pub fn run(&self) -> Result<()> {
        if self.fmt {
            for mut command in self.fmt_commands()? {
                let status = command.status()?;
                ensure!(status.success(), "cargo fmt failed");
            }
            Ok(())
        } else {
            self.run_common(|| self.check_fmt())
        }
    }

    pub fn run_local(&self) -> Result<()> {
        self.run_common(|| {
            if self.check_fmt().is_err() {
                // cargo fmt check failed, this means there is some formatting to do
                // given this task is running locally, let's do it and let our user know
                for mut command in self.fmt_commands()? {
                    let status = command.status()?;
                    ensure!(status.success(), "cargo fmt failed");
                }
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

    fn check_fmt(&self) -> Result<()> {
        for mut command in self.fmt_commands()? {
            let status = command.arg("--check").status()?;
            ensure!(status.success(), "cargo fmt failed");
        }
        Ok(())
    }

    fn fmt_commands(&self) -> Result<Vec<Command>> {
        let mut commands = Vec::new();
        let cargo = which::which("cargo")?;
        let args = ["fmt", "--all", "--", "--config", &RUSTFMT_CONFIG.join(",")];
        let root = &*PKG_PROJECT_ROOT;
        let mut command = Command::new(&cargo);
        command.args(args).current_dir(root);
        commands.push(command);
        if !self.skip_xtask {
            let mut command = Command::new(&cargo);
            command.args(args).current_dir(root.join("xtask"));
            commands.push(command);
        }
        Ok(commands)
    }
}
