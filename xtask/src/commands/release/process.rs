use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

use anyhow::Result;
use dialoguer::{Confirm, Input, Select};
use serde::{Deserialize, Serialize};

#[derive(Debug, clap::Parser)]
pub struct Start {
    #[clap(short = 'v', long = "version")]
    version: Option<String>,
    #[clap(short = 'o', long = "origin")]
    git_origin: Option<String>,
    #[clap(short = 'r', long = "repository")]
    github_repository: Option<String>,
    //#[clap(short = 's', long = "suffix")]
    //prerelease_suffix: Option<String>,
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
                .default(false)
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
            None => Input::new()
                .with_prompt("Version?")
                //FIXME: used for quicker testing, remove before merging
                .default("1.2.3456".to_string())
                .interact_text()?,
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
            prerelease_suffix: String::new(),
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
        let mut process = Process::restore()?;

        loop {
            if !process.run()? {
                return Ok(());
            }
        }
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
            State::ReleasePRCreate => self.create_release_pr(),
            State::PreReleasePRChoose => self.choose_pre_release_pr(),
            State::PreReleasePRCreate => self.create_pre_release_pr(),
            State::PreReleasePRGitAdd => self.git_add_pre_release_pr(),
            State::ReleaseFinish => panic!(),
        }
    }

    fn state_start(&mut self) -> Result<bool> {
        println!(">> Setting up the repository");

        let git = which::which("git")?;

        // step 5
        let _output = std::process::Command::new(&git)
            .args(["checkout", "dev"])
            .status()?;
        let _output = std::process::Command::new(&git)
            .args(["pull", self.git_origin.as_str()])
            .status()?;

        if let Commit::Id(id) = &self.commit {
            let _output = std::process::Command::new(&git)
                .args(["checkout", id])
                .status()?;
        }

        // step 6
        let _output = std::process::Command::new(&git)
            .args(["checkout", "-b", self.version.as_str()])
            .status()?;

        // step 7
        let _output = std::process::Command::new(&git)
            .args([
                "push",
                "--set-upstream",
                self.git_origin.as_str(),
                self.version.as_str(),
            ])
            .status()?;

        self.state = State::ReleasePRCreate;
        self.save()?;

        Ok(true)
    }

    fn create_release_pr(&mut self) -> Result<bool> {
        println!(">> Creating the release PR");

        let gh = which::which("gh")?;

        // step 8
        let pr_text = r#"> **Note**
> **This particular PR must be true-merged to \`main\`.**

* This PR is only ready to review when it is marked as "Ready for Review".  It represents the merge to the \`main\` branch of an upcoming release (version number in the title).
* It will act as a staging branch until we are ready to finalize the release.
* We may cut any number of alpha and release candidate (RC) versions off this branch prior to formalizing it.
* This PR is **primarily a merge commit**, so reviewing every individual commit shown below is **not necessary** since those have been reviewed in their own PR.  However, things important to review on this PR **once it's marked "Ready for Review"**:
    - Does this PR target the right branch? (usually, \`main\`)
    - Are the appropriate **version bumps** and **release note edits** in the end of the commit list (or within the last few commits).  In other words, "Did the 'release prep' PR actually land on this branch?"
    - If those things look good, this PR is good to merge!"#;

        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "pr",
                "create",
                "--draft",
                "--label",
                "release",
                "-B",
                "main",
                "--title",
                &format!("\"release: v{}\"", self.version.as_str()),
                "--body",
                pr_text,
            ])
            .status()?;

        self.state = State::PreReleasePRChoose;
        self.save()?;
        Ok(false)
    }

    fn choose_pre_release_pr(&mut self) -> Result<bool> {
        println!("will choose?");
        let items = vec!["create a prerelease", "finish the release process"];

        let selection = Select::new()
            .with_prompt("Next step?")
            .items(&items)
            .interact()?;

        match selection {
            0 => {
                self.state = State::PreReleasePRCreate;
            }
            1 => {
                self.state = State::ReleaseFinish;
            }
            _ => unreachable!(),
        };
        self.save()?;
        Ok(true)
    }

    fn create_pre_release_pr(&mut self) -> Result<bool> {
        println!(">> Creating the release PR");

        let prerelease_suffix = Input::new()
            .with_prompt(&format!("prerelease suffix? {}-", self.version))
            .with_initial_text(self.prerelease_suffix.clone())
            .interact_text()?;

        let git = which::which("git")?;

        // step 5
        let _output = std::process::Command::new(&git)
            .args(["checkout", &self.version])
            .status()?;
        let _output = std::process::Command::new(&git)
            .args(["pull", &self.git_origin, &self.version])
            .status()?;

        if let Commit::Id(id) = &self.commit {
            let _output = std::process::Command::new(&git)
                .args(["checkout", id])
                .status()?;
        }

        let new_version = format!("{}-{}", self.version, prerelease_suffix);
        println!("prerelease version: {new_version}");
        // step 6
        let prepare = super::Prepare {
            skip_license_check: true,
            pre_release: true,
            version: super::Version::Version(new_version),
        };

        prepare.prepare_release()?;

        self.prerelease_suffix = prerelease_suffix;
        self.state = State::PreReleasePRGitAdd;
        self.save()?;

        Ok(true)
    }

    fn git_add_pre_release_pr(&mut self) -> Result<bool> {
        let git = which::which("git")?;
        // step 7

        println!("please check the changes and add them with `git add -up .`");
        let _output = std::process::Command::new(&git)
            .args(["add", "-up", "."])
            .status()?;

        Ok(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum State {
    Start,
    ReleasePRCreate,
    PreReleasePRChoose,
    PreReleasePRCreate,
    PreReleasePRGitAdd,
    ReleaseFinish,
}
