use std::process::Command;
use std::process::Stdio;
use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use camino::Utf8PathBuf;

use crate::*;

pub struct FederationDemoRunner {
    path: Utf8PathBuf,
}

impl FederationDemoRunner {
    pub fn new() -> Result<Self> {
        let path = PKG_PROJECT_ROOT
            .join("dockerfiles")
            .join("federation-demo")
            .join("federation-demo");

        Ok(Self { path })
    }

    pub fn start_background(&self) -> Result<BackgroundTask> {
        // https://stackoverflow.com/questions/52499617/what-is-the-difference-between-npm-install-and-npm-ci#53325242
        npm!(&self.path => ["clean-install", "--no-progress"]);

        eprintln!("Running federation-demo in background...");
        let mut command = Command::new(which::which("npm")?);

        // Pipe to NULL is required for Windows to not hang
        // https://github.com/rust-lang/rust/issues/45572
        command
            .current_dir(&self.path)
            .args(["run", "start"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let task = BackgroundTask::new(command)?;

        eprintln!("Waiting for federation-demo services and gateway to be ready...");
        loop {
            match reqwest::blocking::get("http://localhost:4100/graphql") {
                Ok(_) => break,
                Err(err) => eprintln!("{}", err),
            }
            sleep(Duration::from_secs(2));
        }

        Ok(task)
    }
}
