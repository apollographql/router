use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

use anyhow::Result;
use dialoguer::{Confirm, Input};
use serde::{Deserialize, Serialize};

#[derive(Debug, clap::Parser)]
pub struct Start {
    #[clap(short = 'v', long = "version")]
    version: Option<String>,
    #[clap(short = 'o', long = "origin")]
    git_origin: Option<String>,
    #[clap(short = 'r', long = "repository")]
    github_repository: Option<String>,
    #[clap(short = 's', long = "suffix")]
    prerelease_suffix: Option<String>,
    #[clap(short = 'c', long = "commit")]
    commit: Option<String>,
}

const STATE_FILE: &str = ".release-state.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Process {
    version: String,
    git_origin: String,
    github_repository: String,
    prerelease_suffix: String,
    commit: Commit,
    state: State,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum Commit {
    Head,
    Id(String),
}

impl Process {
    pub(crate) fn start(arguments: &Start) -> Result<()> {
        // check if a file is already present
        let path = Path::new(STATE_FILE);
        if path.exists() {
            if Confirm::new()
                .with_prompt("A release state file already exists, do you want to remove it and start a new one?")
                .interact()
                ?{
                    std::fs::remove_file(path)?;
                } else {
                    return Ok(());
                }
        }

        // generate the structure
        let version = match &arguments.version {
            Some(v) => v.clone(),
            None => Input::new().with_prompt("Version?").interact_text()?,
        };

        let git_origin = match &arguments.git_origin {
            Some(v) => v.clone(),
            None => Input::new()
                .with_prompt("Git origin?")
                .default("origin".to_string())
                .interact_text()?,
        };

        let github_repository = match &arguments.github_repository {
            Some(v) => v.clone(),
            None => Input::new()
                .with_prompt("Github repository?")
                .default("apollographql/router".to_string())
                .interact_text()?,
        };

        let prerelease_suffix = match &arguments.prerelease_suffix {
            Some(v) => v.clone(),
            None => Input::new()
                .with_prompt(&format!("prerelease suffix? {version}-"))
                .allow_empty(true)
                .interact_text()?,
        };

        let commit = match &arguments.commit {
            Some(v) => v.clone(),
            None => Input::new()
                .with_prompt("Git ref?")
                .default("HEAD".to_string())
                .interact_text()?,
        };

        let commit = if &commit == "HEAD" {
            Commit::Head
        } else {
            Commit::Id(commit)
        };

        let mut process = Self {
            version,
            git_origin,
            github_repository,
            prerelease_suffix,
            commit,
            state: State::Start,
        };

        // store the file
        println!("process: {:#?}", process);
        process.save()?;

        // start asking questions
        loop {
            if !process.run()? {
                return Ok(());
            }
        }
    }

    pub(super) fn cont() -> Result<()> {
        let process = Process::restore()?;

        Ok(())
    }

    fn save(&self) -> Result<()> {
        let path = Path::new(STATE_FILE);

        let serialized = serde_json::to_string_pretty(&self)?;
        let mut file = File::create(path)?;
        file.write_all(serialized.as_bytes())?;
        Ok(())
    }

    fn restore() -> Result<Self> {
        let path = Path::new(STATE_FILE);

        let mut file = File::open(path)?;
        let mut data = String::new();
        file.read_to_string(&mut data)?;

        Ok(serde_json::from_str(&data)?)
    }

    fn run(&mut self) -> Result<bool> {
        match self.state {
            State::Start => self.state_start(),
        }
    }

    fn state_start(&mut self) -> Result<bool> {
        let git = which::which("git")?;
        let output = std::process::Command::new(&git)
            .args(["checkout", "dev"])
            .status()?;
        let output = std::process::Command::new(&git)
            .args(["pull", self.git_origin.as_str()])
            .status()?;

        if let Commit::Id(id) = &self.commit {
            let output = std::process::Command::new(&git)
                .args(["checkout", id])
                .status()?;
        }

        let output = std::process::Command::new(&git)
            .args(["checkout", "-b", self.version.as_str()])
            .status()?;

        Ok(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum State {
    Start,
}
