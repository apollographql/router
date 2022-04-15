use crate::*;
use anyhow::Result;
use camino::Utf8PathBuf;
use std::path::Path;
use std::{
    process::{Command, Stdio},
    thread::sleep,
    time::Duration,
};

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
        let mut command = Command::new(Path::new("jaeger/jaeger-all-in-one"));
        command
            .current_dir(&self.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
