use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use console::style;
use dialoguer::Confirm;
use dialoguer::Input;
use dialoguer::Select;
use serde::Deserialize;
use serde::Serialize;

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
    final_pr_prepared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum Commit {
    Head,
    Id(String),
}

impl Process {
    pub(crate) fn start(arguments: &Start) -> Result<()> {
        println!("{}", style("Starting release process").bold().bright());
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
            final_pr_prepared: false,
        };

        // store the file
        println!("{}: {:?}", style("process").bold().bright(), process);

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
            State::ReleaseFinalPRCreate => self.create_final_release_pr(),
            State::ReleaseFinalPRGitAdd => self.git_add_final_release_pr(),
            State::ReleaseFinalPRMerge => self.merge_final_release_pr(),
            State::ReleaseFinalPRMerge2 => self.merge_final_release_pr2(),
            State::WaitForMergeToMain => self.tag_and_release(),
            State::WaitForReleasePublished => self.update_release_notes(),
        }
    }

    fn state_start(&mut self) -> Result<bool> {
        println!("{}", style("Setting up the repository").bold().bright());

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
        println!("{}", style("Creating the release PR").bold().bright());

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
                &format!("release: v{}", self.version.as_str()),
                "--body",
                pr_text,
            ])
            .status()?;

        self.state = State::PreReleasePRChoose;
        self.save()?;
        Ok(false)
    }

    fn choose_pre_release_pr(&mut self) -> Result<bool> {
        println!("{}", style("Select next release step").bold().bright());

        if !self.final_pr_prepared {
            let items = vec!["create a prerelease", "create the final release PR"];

            let selection = Select::new()
                .with_prompt("Next step?")
                .items(&items)
                .interact()?;

            match selection {
                0 => {
                    self.state = State::PreReleasePRCreate;
                }
                1 => {
                    self.state = State::ReleaseFinalPRCreate;
                }
                _ => unreachable!(),
            };
            self.save()?;
            Ok(true)
        } else {
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
                    self.state = State::ReleaseFinalPRMerge;
                }
                _ => unreachable!(),
            };
            self.save()?;
            Ok(true)
        }
    }

    fn create_pre_release_pr(&mut self) -> Result<bool> {
        println!("{}", style("Creating the pre release PR").bold().bright());

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
        println!(
            "{} {new_version}",
            style("prerelease version: ").bold().bright()
        );

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
        println!(
            "{}",
            style("please check the changes and add them with `git add -up .`")
                .bold()
                .bright()
        );
        let _output = std::process::Command::new(&git)
            .args(["add", "-up", "."])
            .status()?;

        // step 8
        if Confirm::new()
            .with_prompt("Commit the changes and build the prerelease?")
            .default(false)
            .interact()?
        {
            let prerelease_version = format!("{}-{}", self.version, self.prerelease_suffix);

            let _output = std::process::Command::new(&git)
                .args([
                    "commit",
                    "-m",
                    &format!("prep release: v{}", prerelease_version),
                ])
                .status()?;

            //step 9
            let _output = std::process::Command::new(&git)
                .args(["push", &self.git_origin, &self.version])
                .status()?;

            // step 10
            let _output = std::process::Command::new(&git)
                .args([
                    "tag",
                    "-a",
                    &format!("v{}", prerelease_version),
                    "-m",
                    &prerelease_version,
                ])
                .status()?;
            let _output = std::process::Command::new(&git)
                .args([
                    "push",
                    &self.git_origin,
                    &self.version,
                    &format!("v{}", prerelease_version),
                ])
                .status()?;

            // step 11
            println!("{}\ncargo publish -p apollo-federation@{prerelease_version}\ncargo publish -p apollo-router@{prerelease_version}", style("publish the crates:").bold().bright());

            self.state = State::PreReleasePRChoose;
            self.save()?;
        } else {
            return Ok(false);
        }

        Ok(false)
    }

    fn create_final_release_pr(&mut self) -> Result<bool> {
        println!("{}", style("Creating the final release PR").bold().bright());

        let git = which::which("git")?;

        // step 4
        let _output = std::process::Command::new(&git)
            .args(["checkout", &self.version])
            .status()?;
        let _output = std::process::Command::new(&git)
            .args(["pull", &self.git_origin, &self.version])
            .status()?;

        //step 5
        //git checkout -b "prep-${APOLLO_ROUTER_RELEASE_VERSION}"
        let _output = std::process::Command::new(&git)
            .args(["checkout", "-b", &format!("prep-{}", self.version)])
            .status()?;

        // step 6
        // cargo xtask release prepare $APOLLO_ROUTER_RELEASE_VERSION
        let prepare = super::Prepare {
            skip_license_check: true,
            pre_release: false,
            version: super::Version::Version(self.version.clone()),
        };

        prepare.prepare_release()?;

        self.state = State::ReleaseFinalPRGitAdd;
        self.save()?;

        println!(
            "{}\n{}",
            style("prep release branch created").bold().bright(),
            style("**MANUALLY CHECK AND UPDATE** the `federation-version-support.mdx` to make sure it shows the version of Federation which is included in the `router-bridge` that ships with this version of Router.\n This can be obtained by looking at the version of `router-bridge` in `apollo-router/Cargo.toml` and taking the number after the `+` (e.g., `router-bridge@0.2.0+v2.4.3` means Federation v2.4.3).").bold().bright()
        );

        println!("{}", style(r#"Make local edits to the newly rendered `CHANGELOG.md` entries to do some initial editoral.

        These things should have *ALWAYS* been resolved earlier in the review process of the PRs that introduced the changes, but they must be double checked:
    
         - There are no breaking changes.
         - Entries are in categories (e.g., Fixes vs Features) that make sense.
         - Titles stand alone and work without their descriptions.
         - You don't need to read the title for the description to make sense.
         - Grammar is good.  (Or great! But don't let perfect be the enemy of good.)
         - Formatting looks nice when rendered as markdown and follows common convention."#).bold().bright());

        Ok(false)
    }

    fn git_add_final_release_pr(&mut self) -> Result<bool> {
        let git = which::which("git")?;

        // step 11
        println!(
            "{}",
            style("please check the changes and add them with `git add -up .`")
                .bold()
                .bright()
        );

        let _output = std::process::Command::new(&git)
            .args(["add", "-up", "."])
            .status()?;

        let _output = std::process::Command::new(&git)
            .args(["commit", "-m", &format!("prep release: v{}", self.version)])
            .status()?;

        //step 14
        //    git push --set-upstream "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "prep-${APOLLO_ROUTER_RELEASE_VERSION}"
        let _output = std::process::Command::new(&git)
            .args([
                "push",
                "--set-upstream",
                &self.git_origin,
                &format!("prep-{}", self.version),
            ])
            .status()?;

        //Step 15
        //FIXME: replace this step with a rinja template
        let perl = which::which("perl")?;
        let output = std::process::Command::new(&perl)
            .args([
                "0777",
                "-sne",
                r#"print "$1\n" if m{
                (?:\#\s               # Look for H1 Markdown (line starting with "\# ")
                \[v?\Q$version\E\]    # ...followed by [$version] (optionally with a "v")
                                      #    since some versions had that in the past.
                \s.*?\n$)             # ... then "space" until the end of the line.
                \s*                   # Ignore PRE-entry-whitespace
                (.*?)                 # Capture the ACTUAL body of the release.  But do it
                                      # in a non-greedy way, leading us to stop when we
                                      # reach the next version boundary/heading.
                \s*                   # Ignore POST-entry-whitespace
                (?=^\#\s\[[^\]]+\]\s) # Once again, look for a version boundary.  This is
                                      # the same bit at the start, just on one line.
              }msx"#,
                "--",
                "-version",
                &self.version,
                "CHANGELOG.md",
            ])
            .output()?;

        let mut f = std::fs::File::create("this_release.md")?;
        f.write_all(&output.stdout)?;

        //step 16
        let apollo_prep_release_header = format!(
            r#"> **Note**
>
> When approved, this PR will merge into **the \`{}\` branch** which will — upon being approved itself — merge into \`main\`.
>
> **Things to review in this PR**:
>  - Changelog correctness (There is a preview below, but it is not necessarily the most up to date.  See the _Files Changed_ for the true reality.)
>  - Version bumps
>  - That it targets the right release branch (\`${}\` in this case!).
>
---

{}"#,
            &self.version,
            &self.version,
            std::str::from_utf8(&output.stdout)?
        );

        //echo "${apollo_prep_release_header}\n${apollo_prep_release_notes}" | gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr create -B "${APOLLO_ROUTER_RELEASE_VERSION}" --title "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}" --body-file -

        let gh = which::which("gh")?;

        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "pr",
                "create",
                "-B",
                &self.version,
                "--title",
                &format!("prep release: v{}", self.version.as_str()),
                "--body",
                &apollo_prep_release_header,
            ])
            .status()?;

        self.state = State::PreReleasePRChoose;
        self.final_pr_prepared = true;
        self.save()?;
        Ok(false)
    }

    fn merge_final_release_pr(&mut self) -> Result<bool> {
        println!("{}", style("Merging the final release PR").bold().bright());

        let gh = which::which("gh")?;

        // step 4
        //    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge --squash --body "" -t "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}" "prep-${APOLLO_ROUTER_RELEASE_VERSION}"

        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                &self.github_repository,
                "pr",
                "merge",
                "--squash",
                "--body",
                "",
                "-t",
                &format!("prep release: v{}", self.version),
                &format!("prep-{}", self.version),
            ])
            .status()?;

        self.state = State::ReleaseFinalPRMerge2;
        self.save()?;

        //FIXME: can we check the PR status with the gh command?
        println!(
            "{}",
            style("Wait for the pre PR to merge into the release PR")
                .bold()
                .bright()
        );

        Ok(false)
    }

    fn merge_final_release_pr2(&mut self) -> Result<bool> {
        let git = which::which("git")?;

        // step 5
        let _output = std::process::Command::new(&git)
            .args(["checkout", &self.version])
            .status()?;
        let _output = std::process::Command::new(&git)
            .args(["pull", &self.git_origin, &self.version])
            .status()?;

        // step 6
        // gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr ready "${APOLLO_ROUTER_RELEASE_VERSION}"
        let gh = which::which("gh")?;
        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "pr",
                "ready",
                &self.version,
            ])
            .status()?;
        println!("{}", style("release PR marked as ready").bold().bright());

        // step 7
        // gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge --merge --body "" -t "release: v${APOLLO_ROUTER_RELEASE_VERSION}" --auto "${APOLLO_ROUTER_RELEASE_VERSION}"
        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "pr",
                "merge",
                "--merge",
                "--body",
                "",
                "-t",
                &format!("release: v{}", self.version),
                "--auto",
                &self.version,
            ])
            .status()?;

        println!(
            "{}",
            style("Wait for the release PR to merge into main")
                .bold()
                .bright()
        );

        self.state = State::WaitForMergeToMain;
        self.save()?;

        Ok(false)
    }

    fn tag_and_release(&mut self) -> Result<bool> {
        println!("{}", style("Tagging and releasing").bold().bright());

        let git = which::which("git")?;

        // step 9
        // git checkout main && \
        // git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" && \
        // git tag -a "v${APOLLO_ROUTER_RELEASE_VERSION}" -m "${APOLLO_ROUTER_RELEASE_VERSION}" && \
        // git push "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "v${APOLLO_ROUTER_RELEASE_VERSION}"
        let _output = std::process::Command::new(&git)
            .args(["checkout", "main"])
            .status()?;
        let _output = std::process::Command::new(&git)
            .args(["pull", &self.git_origin])
            .status()?;
        let _output = std::process::Command::new(&git)
            .args([
                "tag",
                "-a",
                &format!("v{}", self.version),
                "-m",
                &self.version,
            ])
            .status()?;
        let _output = std::process::Command::new(&git)
            .args(["push", &self.git_origin, &format!("v{}", self.version)])
            .status()?;

        //step 10: reconciliation PR
        //gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr create --title "Reconcile \`dev\` after merge to \`main\` for v${APOLLO_ROUTER_RELEASE_VERSION}"
        // -B dev -H main --body "Follow-up to the v${APOLLO_ROUTER_RELEASE_VERSION} being officially released, bringing version bumps and changelog updates into the \`dev\` branch."
        let gh = which::which("gh")?;
        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "pr",
                "create",
                "--title",
                &format!("Reconcile `dev` after merge to `main` for v{}", self.version),
                "-B", "dev", "-H", "main", "--body",
              &format!("Follow-up to the v{} being officially released, bringing version bumps and changelog updates into the `dev` branch.", self.version)
            ])
            .status()?;
        println!("{}", style("dev reconciliation PR created").bold().bright());

        // step 11: mark the PR as automerge
        //  APOLLO_RECONCILE_PR_URL=$(gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr list --state open --base dev --head main --json url --jq '.[-1] | .url')
        // test -n "${APOLLO_RECONCILE_PR_URL}" && \
        // gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge --merge --auto "${APOLLO_RECONCILE_PR_URL}"
        let output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "pr",
                "list",
                "--state",
                "open",
                "--base",
                "dev",
                "--head",
                "main",
                "--json",
                "url",
                "--jq",
                ".[-1] | .url",
            ])
            .output()?;
        let url = std::str::from_utf8(&output.stdout)?;
        println!(
            "{}: {url}",
            style("reconciliation PR URL: ").bold().bright()
        );

        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "pr",
                "merge",
                "--merge",
                "--auto",
                url.trim(),
            ])
            .status()?;

        println!("{}", style("🗣️ **Solicit approval from the Router team, wait for the reconciliation PR to pass CI and auto-merge into `dev`**").bold().bright());
        println!("{}", style("⚠️ **Wait for `publish_github_release` on CircleCI to finish on this job before continuing.** ⚠️").bold().bright());

        self.state = State::WaitForReleasePublished;
        self.save()?;

        Ok(false)
    }

    fn update_release_notes(&self) -> Result<bool> {
        println!("{}", style("Updating release notes").bold().bright());

        // step 15
        //FIXME: replace this step with a rinja template
        let perl = which::which("perl")?;
        let output = std::process::Command::new(&perl)
            .args([
                "0777",
                "-sne",
                r#"print "$1\n" if m{
                (?:\#\s               # Look for H1 Markdown (line starting with "\# ")
                \[v?\Q$version\E\]    # ...followed by [$version] (optionally with a "v")
                                      #    since some versions had that in the past.
                \s.*?\n$)             # ... then "space" until the end of the line.
                \s*                   # Ignore PRE-entry-whitespace
                (.*?)                 # Capture the ACTUAL body of the release.  But do it
                                      # in a non-greedy way, leading us to stop when we
                                      # reach the next version boundary/heading.
                \s*                   # Ignore POST-entry-whitespace
                (?=^\#\s\[[^\]]+\]\s) # Once again, look for a version boundary.  This is
                                      # the same bit at the start, just on one line.
              }msx"#,
                "--",
                "-version",
                &self.version,
                "CHANGELOG.md",
            ])
            .output()?;

        let mut f = std::fs::File::create("this_release.md")?;
        f.write_all(&output.stdout)?;

        //step 16
        //perl -pi -e 's/\[@([^\]]+)\]\([^)]+\)/@\1/g' this_release.md
        let _output = std::process::Command::new(&perl)
            .args([
                "-pi",
                "-e",
                r#"s/\[@([^\]]+)\]\([^)]+\)/@\1/g"#,
                "this_release.md",
            ])
            .status()?;

        // step 17
        // gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" release edit v"${APOLLO_ROUTER_RELEASE_VERSION}" -F ./this_release.md
        let gh = which::which("gh")?;
        let _output = std::process::Command::new(&gh)
            .args([
                "--repo",
                self.github_repository.as_str(),
                "release",
                "edit",
                &format!("v{}", self.version),
                "-F",
                "./this_release.md",
            ])
            .status()?;

        // step 18
        println!(
            "{}\ncargo publish -p apollo-federation@{}\ncargo publish -p apollo-router@{}",
            style("manually publish the crates:").bold().bright(),
            self.version,
            self.version
        );

        // the release process is now finished, remove the release file
        let path = Path::new(STATE_FILE);
        std::fs::remove_file(path)?;
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
    ReleaseFinalPRCreate,
    ReleaseFinalPRGitAdd,
    ReleaseFinalPRMerge,
    ReleaseFinalPRMerge2,
    WaitForMergeToMain,
    WaitForReleasePublished,
}
