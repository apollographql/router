use std::process::Stdio;

use anyhow::{ensure, Result};
use structopt::StructOpt;
use xtask::*;

const TEST_DEFAULT_ARGS: &[&str] = &["test", "--locked"];

#[derive(Debug, StructOpt)]
pub struct Test {
    /// Do not start federation demo (deprecated, this is the default now).
    #[structopt(long, conflicts_with = "with-demo")]
    no_demo: bool,

    /// Do start the federation demo (without docker).
    ///
    /// To speed up the process, the project will be compiled in background
    /// while federation-demo is booting up. If you want to disable this,
    /// use the --no-pre-compile flag.
    #[structopt(long, conflicts_with = "no-demo")]
    with_demo: bool,

    /// Do not start the project's compilation in background while federation
    /// demo is booting up (requires --with-demo).
    #[structopt(long, conflicts_with = "no-demo")]
    no_pre_compile: bool,
}

impl Test {
    pub fn run(&self) -> Result<()> {
        ensure!(
            !(self.no_demo && self.with_demo),
            "--no-demo and --with-demo are mutually exclusive",
        );

        // NOTE: it worked nicely on GitHub Actions but it hangs on CircleCI on Windows
        let _guard: Box<dyn std::any::Any> = if !std::env::var("CIRCLECI")
            .ok()
            .unwrap_or_default()
            .is_empty()
            && cfg!(windows)
        {
            eprintln!("Not running federation-demo because it makes the step hang on Circle CI.");
            Box::new(())
        } else if self.no_demo {
            eprintln!("Flag --no-demo is the default now. Not running federation-demo.");
            Box::new(())
        } else if !self.with_demo {
            eprintln!("Not running federation-demo.");
            Box::new(())
        } else {
            let mut maybe_pre_compile = if !self.no_pre_compile {
                eprintln!("Starting background process to pre-compile the tests while federation-demo prepares...");
                Some(
                    std::process::Command::new(which::which("cargo")?)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .args(TEST_DEFAULT_ARGS)
                        .arg("--no-run")
                        .spawn()?,
                )
            } else {
                None
            };

            let demo = FederationDemoRunner::new()?;
            let guard = demo.start_background()?;

            if let Some(sub_process) = maybe_pre_compile.as_mut() {
                eprintln!("Waiting for background process that pre-compiles the test to finish...");
                sub_process.wait()?;
            }

            Box::new((demo, guard))
        };

        eprintln!("Running tests");
        cargo!(TEST_DEFAULT_ARGS);

        Ok(())
    }
}
