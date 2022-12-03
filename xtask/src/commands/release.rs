use anyhow::{anyhow, Result};
use itertools::Itertools;
use octorust::types::{
    IssuesCreateMilestoneRequest, IssuesListMilestonesSort, IssuesListState, IssuesUpdateRequest,
    Milestone, Order, PullsCreateRequest, State, TitleOneOf,
};
use octorust::Client;
use std::process::Command;
use structopt::StructOpt;
use tap::TapFallible;
use walkdir::WalkDir;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Release {
    /// Dry run, don't commit the changes and create the PR.
    #[structopt(long)]
    dry_run: bool,

    /// The new version that is being created.
    #[structopt(long)]
    version: String,
}

macro_rules! git {
    ($( $i:expr ),*) => {
        let git = which::which("git")?;
        let result = Command::new(git).args([$( $i ),*]).status()?;
        if !result.success() {
            return Err(anyhow!("git {}", [$( $i ),*].join(",")));
        }
    };
}

macro_rules! replace_in_file {
    ($path:expr, $regex:expr, $replacement:expr) => {
        let before = std::fs::read_to_string($path)?;
        let re = regex::Regex::new(&format!("(?m){}", $regex))?;
        let after = re.replace_all(&before, $replacement);
        std::fs::write($path, &after.as_ref())?;
    };
}

impl Release {
    pub fn run(&self) -> Result<()> {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let github = octorust::Client::new(
                    "router-release".to_string(),
                    octorust::auth::Credentials::Token(
                        std::env::var("GITHUB_TOKEN")
                            .expect("GITHUB_TOKEN env variable must be set"),
                    ),
                )?;
                self.switch_to_release_branch()?;
                self.assign_issues_to_milestone(&github).await?;
                self.update_cargo_tomls()?;
                self.update_install_script()?;
                self.update_docs()?;
                self.update_helm_charts()?;
                self.docker_files()?;
                self.finalize_changelog()?;
                self.update_lock()?;
                self.check_compliance()?;
                if !self.dry_run {
                    self.create_release_pr(&github).await?;
                }

                Ok(())
            })
    }

    /// Create a new branch "#.#.#" where "#.#.#" is this release's version
    /// (release) or "#.#.#-rc.#" (release candidate)
    fn switch_to_release_branch(&self) -> Result<()> {
        println!("creating release branch");
        git!("fetch", "origin", &format!("dev:{}", self.version));
        git!("checkout", &self.version);
        Ok(())
    }

    /// Go through NEXT_CHANGELOG.md find all issues and assign to the milestone.
    /// Any PR that doesn't have an issue assign to the milestone.
    async fn assign_issues_to_milestone(&self, github: &Client) -> Result<()> {
        let change_log = std::fs::read_to_string("./NEXT_CHANGELOG.md")?;

        let re =
            regex::Regex::new(r"(?ms)https://github.com/apollographql/router/(pull|issues)/(\d+)")?;

        println!("searching for milestone v{}", self.version);
        let milestone = self.get_or_create_milestone(&github).await?;
        let mut errors_encountered = false;
        for (issues_or_pull, number) in re
            .captures_iter(&change_log)
            .map(|m| {
                (
                    m.get(1).expect("expected issues or pull").as_str(),
                    m.get(2).expect("expected issue or pull number").as_str(),
                )
            })
            .sorted()
            .dedup()
        {
            if let Err(e) = self
                .handle_issue_or_pr(&github, &milestone, issues_or_pull, number)
                .await
            {
                eprintln!("{}", e);
                errors_encountered = true;
            }
        }
        if errors_encountered {
            return Err(anyhow!("errors encountered, aborting"));
        }
        Ok(())
    }

    async fn get_or_create_milestone(&self, github: &Client) -> Result<Milestone> {
        Ok(
            match github
                .issues()
                .list_milestones(
                    "apollographql",
                    "router",
                    IssuesListState::Open,
                    IssuesListMilestonesSort::FallthroughString,
                    Order::FallthroughString,
                    30,
                    1,
                )
                .await?
                .into_iter()
                .find(|m| m.title == format!("v{}", self.version))
            {
                Some(milestone) => milestone,
                None => {
                    println!("milestone not found, creating...");
                    if !self.dry_run {
                        github
                            .issues()
                            .create_milestone(
                                "apollographql",
                                "router",
                                &IssuesCreateMilestoneRequest {
                                    description: format!("Release v{}", self.version),
                                    due_on: None,
                                    state: Some(State::Open),
                                    title: format!("v{}", self.version),
                                },
                            )
                            .await
                            .tap_err(|_| eprintln!("Failed to create milestone"))?
                    } else {
                        Milestone {
                            closed_at: None,
                            closed_issues: 0,
                            created_at: None,
                            creator: None,
                            description: "".to_string(),
                            due_on: None,
                            html_url: "".to_string(),
                            id: 0,
                            labels_url: "".to_string(),
                            node_id: "".to_string(),
                            number: 0,
                            open_issues: 0,
                            state: Default::default(),
                            title: "".to_string(),
                            updated_at: None,
                            url: "".to_string(),
                        }
                    }
                }
            },
        )
    }

    async fn handle_issue_or_pr(
        &self,
        github: &Client,
        milestone: &Milestone,
        issues_or_pull: &str,
        number: &str,
    ) -> Result<()> {
        match issues_or_pull {
            "issues" => {
                let issue = github
                    .issues()
                    .get("apollographql", "router", number.parse()?)
                    .await
                    .tap_err(|_| {
                        eprintln!(
                            "could not find issue {}, there is an error in NEXT_CHANGELOG.md",
                            number
                        )
                    })?;
                match issue.milestone {
                    None => {
                        println!("assigning milestone to https://github.com/apollographql/router/issues/{}", number);
                        self.update_milestone(github, &milestone, issue.number)
                            .await?;
                    }
                    Some(issue_milestone) if issue_milestone.id != milestone.id => {
                        return Err(anyhow!("issue https://github.com/apollographql/router/issues/{} was assigned to an existing milestone", number));
                    }
                    _ => {}
                }
                if issue.assignees.is_empty() {
                    return Err(anyhow!(
                        "https://github.com/apollographql/router/issue/{} has no assignee",
                        number
                    ));
                }
            }
            "pull" => {
                let pull = github
                    .pulls()
                    .get("apollographql", "router", number.parse()?)
                    .await
                    .tap_err(|_| {
                        eprintln!(
                            "could not find PR {}, there is an error in NEXT_CHANGELOG.md",
                            number
                        )
                    })?;
                match pull.milestone {
                    None => {
                        println!(
                            "assigning milestone to https://github.com/apollographql/router/pull/{}",
                            number
                        );
                        self.update_milestone(github, &milestone, pull.number)
                            .await?;
                    }
                    Some(pull_milestone) if pull_milestone.id != milestone.id => {
                        return Err(anyhow!("issue https://github.com/apollographql/router/pull/{} was assigned to an existing milestone", number));
                    }
                    _ => {}
                }
                if pull.assignees.is_empty() {
                    return Err(anyhow!(
                        "https://github.com/apollographql/router/pull/{} has no assignee",
                        number
                    ));
                }
                if pull.state == State::Open {
                    return Err(anyhow!(
                        "https://github.com/apollographql/router/pull/{} is still open",
                        number
                    ));
                }
            }
            _ => panic!("expected issues or pull"),
        }
        Ok(())
    }

    async fn update_milestone(
        &self,
        github: &Client,
        milestone: &Milestone,
        issue: i64,
    ) -> Result<()> {
        if !self.dry_run {
            github
                .issues()
                .update(
                    "apollographql",
                    "router",
                    issue,
                    &IssuesUpdateRequest {
                        assignee: "".to_string(),
                        assignees: vec![],
                        body: "".to_string(),
                        labels: vec![],
                        milestone: Some(TitleOneOf::I64(milestone.number)),
                        state: None,
                        title: None,
                    },
                )
                .await?;
        }
        Ok(())
    }

    /// Update the `version` in `*/Cargo.toml` (do not forget the ones in scaffold templates).
    /// Update the `apollo-router` version in the `dependencies` sections of the `Cargo.toml` files in `apollo-router-scaffold/templates/**`.
    fn update_cargo_tomls(&self) -> Result<()> {
        println!("updating Cargo.toml files");
        let packages = vec![
            "apollo-router",
            "apollo-router-scaffold",
            "apollo-router-benchmarks",
        ];

        for package in packages {
            cargo!(["set-version", &self.version, "--package", package])
        }
        Ok(())
    }

    /// Update the `PACKAGE_VERSION` value in `scripts/install.sh` (it should be prefixed with `v`!)
    fn update_install_script(&self) -> Result<()> {
        println!("updating install script");
        replace_in_file!(
            "./scripts/install.sh",
            "^PACKAGE_VERSION=.*$",
            format!("PACKAGE_VERSION=\"v{}\"", self.version)
        );
        Ok(())
    }

    /// Update `docker.mdx` and `kubernetes.mdx` with the release version.
    /// Update the kubernetes section of the docs:
    ///   - go to the `helm/chart/router` folder
    ///   - run
    ///   ```helm template --set router.configuration.telemetry.metrics.prometheus.enabled=true  --set managedFederation.apiKey="REDACTED" --set managedFederation.graphRef="REDACTED" --debug .```
    ///   - Paste the output in the `Kubernetes Configuration` example of the `docs/source/containerization/kubernetes.mdx` file
    fn update_docs(&self) -> Result<()> {
        println!("updating docs");
        replace_in_file!(
            "./docs/source/containerization/docker.mdx",
            "with your chosen version. e.g.: `v\\d+.\\d+.\\d+`",
            format!("with your chosen version. e.g.: `v{}`", self.version)
        );
        replace_in_file!(
            "./docs/source/containerization/kubernetes.mdx",
            "router/tree/v\\d+.\\d+.\\d+",
            format!("router/tree/v{}", self.version)
        );
        let helm_chart = String::from_utf8(
            Command::new(which::which("helm")?)
                .current_dir("./helm/chart/router")
                .args([
                    "template",
                    "--set",
                    "router.configuration.telemetry.metrics.prometheus.enabled=true",
                    "--set",
                    "managedFederation.apiKey=\"REDACTED\"",
                    "--set",
                    "managedFederation.graphRef=\"REDACTED\"",
                    "--debug",
                    ".",
                ])
                .output()?
                .stdout,
        )?;

        replace_in_file!(
            "./docs/source/containerization/kubernetes.mdx",
            "^```yaml\n---\n# Source: router/templates/serviceaccount.yaml(.|\n)+?```",
            format!("```yaml\n{}\n```", helm_chart.trim())
        );

        Ok(())
    }

    /// Update `helm/chart/router/README.md` by running this from the repo root: `(cd helm/chart && helm-docs router)`.
    ///   (If not installed, you should [install `helm-docs`](https://github.com/norwoodj/helm-docs))
    fn update_helm_charts(&self) -> Result<()> {
        println!("updating helm chars");
        if !Command::new(which::which("helm-docs")?)
            .current_dir("./helm/chart")
            .args(["helm-docs", "router"])
            .status()?
            .success()
        {
            return Err(anyhow!("failed to generate helm docs"));
        }
        Ok(())
    }
    /// Update the `image` of the Docker image within `docker-compose*.yml` files inside the `dockerfiles` directory.
    fn docker_files(&self) -> Result<()> {
        println!("updating docker files");
        for entry in WalkDir::new("./dockerfiles") {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("docker-compose.")
            {
                replace_in_file!(
                    entry.path(),
                    r"ghcr.io/apollographql/router:v\d+.\d+.\d+",
                    format!("ghcr.io/apollographql/router:v{}", self.version)
                );
            }
        }
        Ok(())
    }

    /// Add a new section in `CHANGELOG.md` with the contents of `NEXT_CHANGELOG.md`
    /// Put a Release date and the version number on the new `CHANGELOG.md` section
    /// Update the version in `NEXT_CHANGELOG.md`.
    /// Clear `NEXT_CHANGELOG.md` leaving only the template.
    fn finalize_changelog(&self) -> Result<()> {
        println!("finalizing changelog");
        let next_changelog = std::fs::read_to_string("./NEXT_CHANGELOG.md")?;
        let changelog = std::fs::read_to_string("./CHANGELOG.md")?;
        let changes_regex =
            regex::Regex::new(r"(?ms)(.*# \[x.x.x\] \(unreleased\) - ....-mm-dd\n)(.*)")?;
        let captures = changes_regex
            .captures(&next_changelog)
            .expect("changelog format was unexpected");
        let template = captures
            .get(1)
            .expect("changelog format was unexpected")
            .as_str();
        let changes = captures
            .get(2)
            .expect("changelog format was unexpected")
            .as_str();

        let update_regex = regex::Regex::new(
            r"(?ms)This project adheres to \[Semantic Versioning v2.0.0\]\(https://semver.org/spec/v2.0.0.html\).\n",
        )?;
        let updated = update_regex.replace(&changelog, format!("This project adheres to [Semantic Versioning v2.0.0](https://semver.org/spec/v2.0.0.html).\n\n# [{}] - {}\n{}\n", self.version, chrono::Utc::now().date_naive(), changes));
        std::fs::write("./CHANGELOG.md", updated.to_string())?;
        std::fs::write("./NEXT_CHANGELOG.md", template.to_string())?;
        Ok(())
    }
    /// Update the license list with `cargo about generate --workspace -o licenses.html about.hbs`.
    ///     (If not installed, you can install `cargo-about` by running `cargo install cargo-about`.)
    /// Run `cargo xtask check-compliance`.
    fn check_compliance(&self) -> Result<()> {
        println!("checking compliance");
        cargo!([
            "about",
            "generate",
            "--workspace",
            "-o",
            "licenses.html",
            "about.hbs"
        ]);
        cargo!(["xtask", "check-compliance"]);
        Ok(())
    }

    /// Run `cargo check` so the lock file gets updated.
    fn update_lock(&self) -> Result<()> {
        println!("updating lock file");
        cargo!(["check"]);
        Ok(())
    }

    /// Create the release PR
    async fn create_release_pr(&self, github: &Client) -> Result<()> {
        println!("creating release PR");
        git!("add", "-u");
        git!("commit", "-m", &format!("release {}", self.version));
        git!("push");
        github
            .pulls()
            .create(
                "apollographql",
                "router",
                &PullsCreateRequest {
                    base: "main".to_string(),
                    body: format!("Release {}", self.version),
                    draft: None,
                    head: self.version.clone(),
                    issue: 0,
                    maintainer_can_modify: None,
                    title: format!("Release {}", self.version),
                },
            )
            .await
            .tap_err(|_| eprintln!("failed to create release PR"))?;
        Ok(())
    }
}
