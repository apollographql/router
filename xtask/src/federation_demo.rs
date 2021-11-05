use crate::*;
use anyhow::Result;
use camino::Utf8PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

pub struct FederationDemoRunner {
    path: Utf8PathBuf,
}

impl FederationDemoRunner {
    pub fn new() -> Result<Self> {
        let path = PKG_PROJECT_ROOT
            .join("examples")
            .join("nodejs")
            .join("federation-demo");

        Ok(Self { path })
    }

    pub fn start_background(&self) -> Result<BackgroundTask> {
        npm!(&self.path => ["install", "--no-progress"]);

        eprintln!("Running federation-demo in background...");
        let mut command = Command::new(which::which("npm")?);
        command
            .current_dir(&self.path)
            .args(["run", "start-services"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let task = BackgroundTask::new(command)?;

        eprintln!("Waiting for service to be ready...");
        loop {
            match reqwest::blocking::get("http://localhost:4000/graphql") {
                Ok(_) => break,
                Err(err) => eprintln!("{}", err),
            }
            sleep(Duration::from_secs(2));
        }

        Ok(task)
    }
}
