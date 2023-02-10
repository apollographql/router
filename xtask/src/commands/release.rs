use std::str::FromStr;

use anyhow::anyhow;
use anyhow::Error;
use anyhow::Result;
use cargo_metadata::MetadataCommand;
use chrono::prelude::Utc;
use git2::Repository;
use octorust::types::PullsCreateRequest;
use octorust::Client;
use structopt::StructOpt;
use tap::TapFallible;
use walkdir::WalkDir;
use xtask::*;

#[derive(Debug, StructOpt)]
pub enum Command {
    /// Prepare a new release
    Prepare(Prepare),
}

impl Command {
    pub fn run(&self) -> Result<()> {
        match self {
            Command::Prepare(command) => command.run(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum Version {
    Major,
    Minor,
    Patch,
    Current,
    Nightly,
    Version(String),
}

type ParseError = &'static str;

impl FromStr for Version {
    type Err = ParseError;
    fn from_str(version: &str) -> Result<Self, Self::Err> {
        Ok(match version {
            "major" => Version::Major,
            "minor" => Version::Minor,
            "patch" => Version::Patch,
            "current" => Version::Current,
            "nightly" => Version::Nightly,
            version => Version::Version(version.to_string()),
        })
    }
}

#[derive(Debug, StructOpt)]
pub struct Prepare {
    /// Release from the current branch rather than creating a new one.
    #[structopt(long)]
    current_branch: bool,

    /// Skip the license check
    #[structopt(long)]
    skip_license_ckeck: bool,

    /// Dry run, don't commit the changes and create the PR.
    #[structopt(long)]
    dry_run: bool,

    /// The new version that is being created OR to bump (major|minor|patch|current).
    version: Version,
}

macro_rules! git {
    ($( $i:expr ),*) => {
        let git = which::which("git")?;
        let result = std::process::Command::new(git).args([$( $i ),*]).status()?;
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

impl Prepare {
    pub fn run(&self) -> Result<()> {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { self.prepare_release().await })
    }

    async fn prepare_release(&self) -> Result<(), Error> {
        self.ensure_pristine_checkout()?;
        self.ensure_prereqs()?;
        let version = self.update_cargo_tomls(&self.version)?;
        let github = octorust::Client::new(
            "router-release".to_string(),
            octorust::auth::Credentials::Token(
                std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN env variable must be set"),
            ),
        )?;
        self.update_lock()?;
        self.check_compliance()?;

        if let Version::Nightly = &self.version {
            println!("Skipping various steps becasuse this is a nightly build.");
        } else {
            self.update_install_script(&version)?;
            self.update_helm_charts(&version)?;
            self.update_docs(&version)?;
            self.docker_files(&version)?;
            self.finalize_changelog(&version)?;

            if !self.dry_run {
                if !self.current_branch {
                    self.switch_to_release_branch(&version)?;
                }

                // This also commits all changes to previously tracked files
                // created by this script.
                self.create_release_pr(&github, &version).await?;
            }
        }

        Ok(())
    }

    fn ensure_pristine_checkout(&self) -> Result<(), anyhow::Error> {
        let git = which::which("git")?;
        let output = std::process::Command::new(git)
            .args(["status", "--untracked-files=no", "--porcelain"])
            .output()?;

        if !output.stdout.is_empty() {
            return Err(anyhow!(
                "git workspace was not clean and requires 'git stash' before releasing"
            ));
        }
        Ok(())
    }

    fn ensure_prereqs(&self) -> Result<()> {
        if which::which("git").is_err() {
            return Err(anyhow!(
                "the 'git' executable could not be found in your PATH"
            ));
        }

        if let Version::Nightly = &self.version {
            println!("Skipping requirement that helm and helm-docs is installed because we're building a nightly that doesn't require those tools.");
        } else {
            if which::which("helm").is_err() {
                return Err(anyhow!("the 'helm' executable could not be found in your PATH.  Install it using the instructions at https://helm.sh/docs/intro/install/ and try again."));
            }

            if which::which("helm-docs").is_err() {
                return Err(anyhow!("the 'helm-docs' executable could not be found in your PATH.  Install it using the instructions at https://github.com/norwoodj/helm-docs#installation and try again."));
            }
        }

        if which::which("cargo-about").is_err() {
            return Err(anyhow!("the 'cargo-about' executable could not be found in your PATH.  Install it by running `cargo install --locked cargo-about"));
        }

        if which::which("cargo-deny").is_err() {
            return Err(anyhow!("the 'cargo-deny' executable could not be found in your PATH.  Install it by running `cargo install --locked cargo-deny"));
        }

        if let Version::Nightly = &self.version {
            println!("Skipping requirement that GITHUB_TOKEN is set in the environment because this is a nightly release which doesn't yet need it.");
        } else if std::env::var("GITHUB_TOKEN").is_err() {
            return Err(anyhow!("the GITHUB_TOKEN environment variable must be set to a valid personal access token prior to starting a release. Obtain a personal access token at https://github.com/settings/tokens which has the 'repo' scope."));
        }
        Ok(())
    }

    /// Create a new branch "#.#.#" where "#.#.#" is this release's version
    /// (release) or "#.#.#-rc.#" (release candidate)
    fn switch_to_release_branch(&self, version: &str) -> Result<()> {
        println!("creating release branch");
        git!("fetch", "origin", &format!("dev:{version}"));
        git!("checkout", version);
        Ok(())
    }

    /// Update the `version` in `*/Cargo.toml` (do not forget the ones in scaffold templates).
    /// Update the `apollo-router` version in the `dependencies` sections of the `Cargo.toml` files in `apollo-router-scaffold/templates/**`.
    fn update_cargo_tomls(&self, version: &Version) -> Result<String> {
        println!("updating Cargo.toml files");
        match version {
            Version::Current => {}
            Version::Major => cargo!([
                "set-version",
                "--bump",
                "major",
                "--package",
                "apollo-router"
            ]),
            Version::Minor => cargo!([
                "set-version",
                "--bump",
                "minor",
                "--package",
                "apollo-router"
            ]),
            Version::Patch => cargo!([
                "set-version",
                "--bump",
                "patch",
                "--package",
                "apollo-router"
            ]),
            Version::Nightly => {
                let head_commit: String = match Repository::open_from_env() {
                    Ok(repo) => {
                        let revspec = repo.revparse("HEAD")?;
                        if revspec.mode().contains(git2::RevparseMode::SINGLE) {
                            let mut full_hash = revspec.from().unwrap().id().to_string();
                            full_hash.truncate(8);
                            full_hash
                        } else {
                            panic!("unexpected rev-parse HEAD");
                        }
                    }
                    Err(e) => panic!("failed to open: {e}"),
                };

                replace_in_file!(
                    "./apollo-router/Cargo.toml",
                    r#"^(?P<existingVersion>version\s*=\s*)"[^"]+""#,
                    format!(
                        "${{existingVersion}}\"0.0.0-nightly.{}+{}\"",
                        Utc::now().format("%Y%m%d"),
                        head_commit
                    )
                );
            }
            Version::Version(version) => {
                cargo!(["set-version", version, "--package", "apollo-router"])
            }
        }

        let metadata = MetadataCommand::new()
            .manifest_path("./apollo-router/Cargo.toml")
            .exec()?;
        let resolved_version = metadata
            .root_package()
            .expect("root package missing")
            .version
            .to_string();

        if let Version::Nightly = version {
            println!("Not changing `apollo-router-scaffold` or `apollo-router-benchmarks` because of nightly build mode.");
        } else {
            let packages = vec!["apollo-router-scaffold", "apollo-router-benchmarks"];
            for package in packages {
                cargo!(["set-version", &resolved_version, "--package", package])
            }
            replace_in_file!(
                "./apollo-router-scaffold/templates/base/Cargo.toml",
                "^apollo-router\\s*=\\s*\"[^\"]+\"",
                format!("apollo-router = \"{resolved_version}\"")
            );
            replace_in_file!(
                "./apollo-router-scaffold/templates/base/xtask/Cargo.toml",
                r#"^apollo-router-scaffold = \{\s*git\s*=\s*"https://github.com/apollographql/router.git",\s*tag\s*=\s*"v[^"]+"\s*\}$"#,
                format!(
                    r#"apollo-router-scaffold = {{ git = "https://github.com/apollographql/router.git", tag = "v{resolved_version}" }}"#
                )
            );
        }

        Ok(resolved_version)
    }

    /// Update the `PACKAGE_VERSION` value in `scripts/install.sh` (it should be prefixed with `v`!)
    fn update_install_script(&self, version: &str) -> Result<()> {
        println!("updating install script");
        replace_in_file!(
            "./scripts/install.sh",
            "^PACKAGE_VERSION=.*$",
            format!("PACKAGE_VERSION=\"v{version}\"")
        );
        Ok(())
    }

    /// Update `docker.mdx` and `kubernetes.mdx` with the release version.
    /// Update the kubernetes section of the docs:
    ///   - go to the `helm/chart/router` folder
    ///   - run
    ///   ```helm template --set router.configuration.telemetry.metrics.prometheus.enabled=true  --set managedFederation.apiKey="REDACTED" --set managedFederation.graphRef="REDACTED" --debug .```
    ///   - Paste the output in the `Kubernetes Configuration` example of the `docs/source/containerization/kubernetes.mdx` file
    fn update_docs(&self, version: &str) -> Result<()> {
        println!("updating docs");
        replace_in_file!(
            "./docs/source/containerization/docker.mdx",
            "with your chosen version. e.g.: `v[^`]+`",
            format!("with your chosen version. e.g.: `v{version}`")
        );
        replace_in_file!(
            "./docs/source/containerization/kubernetes.mdx",
            "https://github.com/apollographql/router/tree/[^/]+/helm/chart/router",
            format!("https://github.com/apollographql/router/tree/v{version}/helm/chart/router")
        );
        let helm_chart = String::from_utf8(
            std::process::Command::new(which::which("helm")?)
                .current_dir("./helm/chart/router")
                .args([
                    "template",
                    "--set",
                    "router.configuration.telemetry.metrics.prometheus.enabled=true",
                    "--set",
                    "managedFederation.apiKey=REDACTED",
                    "--set",
                    "managedFederation.graphRef=REDACTED",
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
    fn update_helm_charts(&self, version: &str) -> Result<()> {
        println!("updating helm charts");

        replace_in_file!(
            "./helm/chart/router/Chart.yaml",
            "^version:.*?$",
            format!("version: {version}")
        );

        replace_in_file!(
            "./helm/chart/router/Chart.yaml",
            "appVersion: \"v[^\"]+\"",
            format!("appVersion: \"v{version}\"")
        );

        if !std::process::Command::new(which::which("helm-docs")?)
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
    fn docker_files(&self, version: &str) -> Result<()> {
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
                    r#"^(?P<indentation>\s+)image:\s*ghcr.io/apollographql/router:v.*$"#,
                    format!("${{indentation}}image: ghcr.io/apollographql/router:v{version}")
                );
            }
        }
        Ok(())
    }

    /// Add a new section in `CHANGELOG.md` with the contents of `NEXT_CHANGELOG.md`
    /// Put a Release date and the version number on the new `CHANGELOG.md` section
    /// Update the version in `NEXT_CHANGELOG.md`.
    /// Clear `NEXT_CHANGELOG.md` leaving only the template.
    fn finalize_changelog(&self, version: &str) -> Result<()> {
        println!("finalizing changelog");
        let next_changelog = std::fs::read_to_string("./NEXT_CHANGELOG.md")?;
        let changelog = std::fs::read_to_string("./CHANGELOG.md")?;
        let changes_regex = regex::Regex::new(
            r"(?ms)(?P<example>^<!-- <KEEP>.*^</KEEP> -->\s*)(?P<newChangelog>.*)?",
        )?;
        let captures = changes_regex
            .captures(&next_changelog)
            .expect("changelog format was unexpected1");

        // There must be a block like this in the CHANGELOG.
        //
        // <!-- <KEEP>
        //   Anything here.  Doesn't matter.
        // </KEEP> -->
        captures.name("example").expect("example block was not found in changelog; see xtask release command source code for expectation of example block");

        let new_changelog_text = captures
            .name("newChangelog")
            .expect("newChangelog was not found, possibly because the format was unexpected")
            .as_str();

        let new_next_changelog = changes_regex.replace(&next_changelog, "${example}");

        let semver_heading = "This project adheres to [Semantic Versioning v2.0.0](https://semver.org/spec/v2.0.0.html).";

        let update_regex =
            regex::Regex::new(format!("(?ms){}\n", regex::escape(semver_heading)).as_str())?;
        let updated = update_regex.replace(
            &changelog,
            format!(
                "{}\n\n# [{}] - {}\n\n{}\n",
                semver_heading,
                version,
                chrono::Utc::now().date_naive(),
                new_changelog_text
            ),
        );
        std::fs::write("./CHANGELOG.md", updated.to_string())?;
        std::fs::write("./NEXT_CHANGELOG.md", new_next_changelog.to_string())?;
        Ok(())
    }
    /// Update the license list with `cargo about generate --workspace -o licenses.html about.hbs`.
    ///     (If not installed, you can install `cargo-about` by running `cargo install cargo-about`.)
    /// Run `cargo xtask check-compliance`.
    fn check_compliance(&self) -> Result<()> {
        println!("checking compliance");
        cargo!(["xtask", "check-compliance"]);
        if !self.skip_license_ckeck {
            println!("updating licenses.html");
            cargo!(["xtask", "licenses"]);
        }
        Ok(())
    }

    /// Run `cargo check` so the lock file gets updated.
    fn update_lock(&self) -> Result<()> {
        println!("updating lock file");
        cargo!(["check"]);
        Ok(())
    }

    /// Create the release PR
    async fn create_release_pr(&self, github: &Client, version: &str) -> Result<()> {
        let git = which::which("git")?;
        let result = std::process::Command::new(git)
            .args(["branch", "--show-current"])
            .output()?;
        if !result.status.success() {
            return Err(anyhow!("failed to get git current branch"));
        }
        let current_branch = String::from_utf8(result.stdout)?;

        println!("creating release PR");
        git!("add", "-u");
        git!("commit", "-m", &format!("release {version}"));
        git!(
            "push",
            "--set-upstream",
            "origin",
            &format!("{}:{}", current_branch.trim(), version)
        );
        github
            .pulls()
            .create(
                "apollographql",
                "router",
                &PullsCreateRequest {
                    base: "main".to_string(),
                    body: format!("Release {version}"),
                    draft: None,
                    head: version.to_string(),
                    issue: 0,
                    maintainer_can_modify: None,
                    title: format!("Release {version}"),
                },
            )
            .await
            .tap_err(|_| eprintln!("failed to create release PR"))?;
        Ok(())
    }
}
