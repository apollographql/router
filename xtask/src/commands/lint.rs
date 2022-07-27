use std::process::Command;
use std::process::Stdio;

use anyhow::ensure;
use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Lint {}

const RUSTFMT_TOOLCHAIN: &str = "nightly-2022-06-26";

const RUSTFMT_CONFIG: &[&str] = &["imports_granularity=Item", "group_imports=StdExternalCrate"];

impl Lint {
    pub fn run(&self) -> Result<()> {
        self.run_common(Self::check_fmt)
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
        Self::install_rustfmt()?;
        fmt()?;
        cargo!(["clippy", "--all", "--all-targets", "--", "-D", "warnings"]);
        cargo!(["doc", "--all", "--no-deps"], env = { "RUSTDOCFLAGS" => "-Dwarnings" });
        Ok(())
    }

    fn install_rustfmt() -> Result<()> {
        let nightly = RUSTFMT_TOOLCHAIN;
        if !output("rustup", &["toolchain", "list"])?
            .lines()
            .any(|line| line.starts_with(nightly))
        {
            let args = ["toolchain", "install", nightly, "--profile", "minimal"];
            run("rustup", &args)?
        }
        let args = ["component", "list", "--installed", "--toolchain", nightly];
        if !output("rustup", &args)?
            .lines()
            .any(|line| line.starts_with("rustfmt"))
        {
            let args = ["component", "add", "rustfmt", "--toolchain", nightly];
            run("rustup", &args)?
        }
        Ok(())
    }

    fn check_fmt() -> Result<()> {
        let status = Self::fmt_command()?.arg("--check").status()?;
        ensure!(status.success(), "cargo fmt check failed");
        Ok(())
    }

    fn fmt_command() -> Result<Command> {
        let mut command = Command::new(which::which("rustup")?);
        command.current_dir(&*PKG_PROJECT_ROOT).args([
            "run",
            RUSTFMT_TOOLCHAIN,
            "cargo",
            "fmt",
            "--all",
            "--",
            "--config",
            &RUSTFMT_CONFIG.join(","),
        ]);
        Ok(command)
    }
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(which::which(program)?).args(args).status()?;
    ensure!(status.success(), "{} failed", program);
    Ok(())
}

fn output(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(which::which(program)?)
        .args(args)
        .stderr(Stdio::piped())
        .output()?;
    ensure!(output.status.success(), "{} failed", program);
    Ok(String::from_utf8(output.stdout)?)
}
