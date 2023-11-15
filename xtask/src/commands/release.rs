use std::str::FromStr;

use anyhow::anyhow;
use anyhow::Error;
use anyhow::Result;
use cargo_metadata::MetadataCommand;
use chrono::prelude::Utc;
use walkdir::WalkDir;
use xtask::*;

use crate::commands::changeset::slurp_and_remove_changesets;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Prepare a new release
    Prepare(Prepare),

    /// Verify that a release is ready to be published
    PreVerify,
}

impl Command {
    pub fn run(&self) -> Result<()> {
        match self {
            Command::Prepare(command) => command.run(),
            Command::PreVerify => PreVerify::run(),
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

#[derive(Debug, clap::Parser)]
pub struct Prepare {
    /// Skip the license check
    #[clap(long)]
    skip_license_check: bool,

    /// The new version that is being created OR to bump (major|minor|patch|current).
    version: Version,
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
        self.update_lock()?;
        self.check_compliance()?;

        if let Version::Nightly = &self.version {
            println!("Skipping various steps because this is a nightly build.");
            // Only update helm charts on specific arch/os/env
            if cfg!(target_arch = "x86_64") && cfg!(target_os = "linux") && cfg!(target_env = "gnu")
            {
                // Update the image repository to use the nightly location
                replace_in_file!(
                    "./helm/chart/router/values.yaml",
                    "^  repository: ghcr.io/apollographql/router$",
                    format!("  repository: ghcr.io/apollographql/nightly/router")
                );

                // Update the version string for nightly builds
                self.update_helm_charts(&version.replace("+", "-"))?;
            }
        } else {
            self.update_install_script(&version)?;
            self.update_helm_charts(&version)?;
            self.update_docs(&version)?;
            self.docker_files(&version)?;
            self.finalize_changelog(&version)?;
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
            if cfg!(target_arch = "x86_64") && cfg!(target_os = "linux") && cfg!(target_env = "gnu")
            {
                if which::which("helm").is_err() {
                    return Err(anyhow!("the 'helm' executable could not be found in your PATH.  Install it using the instructions at https://helm.sh/docs/intro/install/ and try again."));
                }

                if which::which("helm-docs").is_err() {
                    return Err(anyhow!("the 'helm-docs' executable could not be found in your PATH.  Install it using the instructions at https://github.com/norwoodj/helm-docs#installation and try again."));
                }
            }
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
        Ok(())
    }

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
                // Get the first 8 characters of the current commit hash by running
                // the Command::new("git") command.  Be sure to take the output and
                // run that through String::from_utf8(output.stdout) to get exactly
                // an 8 character string.
                let head_commit = std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .output()
                    .expect("failed to execute 'git rev-parse HEAD'")
                    .stdout;

                // If it's empty, then we're in a bad state.
                if head_commit.is_empty() {
                    return Err(anyhow!("failed to get the current commit hash"));
                }

                // Convert it using `String::from_utf8_lossy`, which will turn
                // any funky characters into something really noticeable.
                let head_commit = String::from_utf8_lossy(&head_commit);

                // Just get the first 8 characters, for brevity.
                let head_commit = head_commit.chars().take(8).collect::<String>();

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
        let changelog = std::fs::read_to_string("./CHANGELOG.md")?;

        let semver_heading = "This project adheres to [Semantic Versioning v2.0.0](https://semver.org/spec/v2.0.0.html).";

        let new_changelog = slurp_and_remove_changesets();

        let update_regex =
            regex::Regex::new(format!("(?ms){}\n", regex::escape(semver_heading)).as_str())?;
        let updated = update_regex.replace(
            &changelog,
            format!(
                "{}\n\n# [{}] - {}\n\n{}\n",
                semver_heading,
                version,
                chrono::Utc::now().date_naive(),
                &new_changelog,
            ),
        );
        std::fs::write("./CHANGELOG.md", updated.to_string())?;
        Ok(())
    }
    /// Update the license list with `cargo about generate --workspace -o licenses.html about.hbs`.
    ///     (If not installed, you can install `cargo-about` by running `cargo install cargo-about`.)
    /// Run `cargo xtask check-compliance`.
    fn check_compliance(&self) -> Result<()> {
        println!("checking compliance");
        cargo!(["xtask", "check-compliance"]);
        if !self.skip_license_check {
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
}

struct PreVerify();

impl PreVerify {
    fn run() -> Result<()> {
        let version = format!("v{}", *PKG_VERSION);

        // Get the git tag name as a string
        let tags_output = std::process::Command::new("git")
            .args(["describe", "--tags", "--exact-match"])
            .output()
            .map_err(|e| {
                anyhow!(
                    "failed to execute 'git describe --tags --exact-match': {}",
                    e
                )
            })?
            .stdout;
        let tags_raw = String::from_utf8_lossy(&tags_output);
        let tags_list = tags_raw
            .split("\n")
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>();

        // If the tags contains the version, then we're good
        if tags_list.is_empty() {
            return Err(anyhow!(
                "release cannot be performed because current git tree is not tagged"
            ));
        }
        if !tags_list.contains(&version.as_str()) {
            return Err(anyhow!(
                "the git tree tags {{{}}} does not contain the version {} from the Cargo.toml",
                tags_list.join(", "),
                version
            ));
        }
        Ok(())
    }
}
