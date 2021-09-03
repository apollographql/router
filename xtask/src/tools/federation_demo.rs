use crate::tools::{BackgroundTask, Runner};
use crate::utils::{self, PKG_PROJECT_ROOT};

use anyhow::Result;
use camino::Utf8PathBuf;
use std::thread::sleep;
use std::time::Duration;

pub(crate) struct FederationDemoRunner {
    npm: Runner,
    path: Utf8PathBuf,
}

impl FederationDemoRunner {
    pub(crate) fn new(verbose: bool) -> Result<Self> {
        let npm = Runner::new("npm", verbose)?;
        let path = PKG_PROJECT_ROOT
            .join("dockerfiles")
            .join("federation-demo")
            .join("federation-demo");

        Ok(Self { npm, path })
    }

    pub(crate) fn start_background(&self) -> Result<BackgroundTask> {
        self.npm
            .exec(&["install", "--no-progress"], &self.path, None)?;
        let task = self
            .npm
            .exec_background(&["run", "start"], &self.path, None)?;

        utils::info("Waiting for service to be ready...");
        loop {
            match reqwest::blocking::get("http://localhost:4000/graphql") {
                Ok(_) => break,
                Err(err) => utils::info(&err.to_string()),
            }
            sleep(Duration::from_secs(2));
        }

        Ok(task)
    }
}
