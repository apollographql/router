use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use camino::Utf8PathBuf;

use crate::*;

pub struct JaegerRunner {
    path: Utf8PathBuf,
}

impl JaegerRunner {
    pub fn new() -> Result<Self> {
        let path = PKG_PROJECT_ROOT.to_path_buf();

        Ok(Self { path })
    }

    pub fn start_background(&self) -> Result<BackgroundTask> {
        eprintln!("Running jaeger in background...");
        let jaeger_path = if cfg!(windows) {
            PathBuf::from_iter(["jaeger", "jaeger-all-in-one.exe"])
        } else {
            PathBuf::from_iter(["jaeger", "jaeger-all-in-one"])
        };

        let mut command = Command::new(jaeger_path);

        // Pipe to NULL is required for Windows to not hang
        // https://github.com/rust-lang/rust/issues/45572
        command
            .current_dir(&self.path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if cfg!(windows) {}

        let task = BackgroundTask::new(command)?;

        loop {
            match reqwest::blocking::get("http://localhost:16686") {
                Ok(_) => break,
                Err(err) => eprintln!("{}", err),
            }
            sleep(Duration::from_secs(2));
        }

        Ok(task)
    }
}
