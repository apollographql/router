use anyhow::{anyhow, Result};
use std::process::Command;
use structopt::StructOpt;
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
    ($( $i:expr ),*) => {
        let git = which::which("git")?;
        let result = Command::new(git).args([$( $i ),*]).status()?;
        if !result.success() {
            return Err(anyhow!("git {}", [$( $i ),*].join(",")));
        }
    };
}

impl Release {
    pub fn run(&self) -> Result<()> {
        //self.switch_to_release_branch()?;
        self.assign_issues_to_milestone()?;
        self.update_cargo_tomls()?;
        self.update_install_script()?;
        self.update_docs()?;
        self.update_helm_charts()?;
        self.docker_files()?;
        self.finalize_changelog()?;
        self.update_lock()?;
        self.check_compliance()?;
        if !self.dry_run {
            self.create_release_pr()?;
        }
        git!("checkout", "dev");
        Ok(())
    }

    /// Create a new branch "#.#.#" where "#.#.#" is this release's version
    /// (release) or "#.#.#-rc.#" (release candidate)
    fn switch_to_release_branch(&self) -> Result<()> {
        println!("Creating release branch");
        git!("fetch", "origin", &format!("dev:{}", self.version));
        git!("checkout", &self.version);
        Ok(())
    }

    /// Go through NEXT_CHANGELOG.md fina all issues and assign to the milestone.
    /// Any PR that doesn't have an issue assign to the milestone.
    fn assign_issues_to_milestone(&self) -> Result<()> {
        // let github = octo::Client::new(
        //     String::from("user-agent-name"),
        //     Credentials::Token(String::from("personal-access-token")),
        // );
        Ok(())
    }

    /// Update the `version` in `*/Cargo.toml` (do not forget the ones in scaffold templates).
    /// Update the `apollo-router` version in the `dependencies` sections of the `Cargo.toml` files in `apollo-router-scaffold/templates/**`.
    fn update_cargo_tomls(&self) -> Result<()> {
        println!("Updating Cargo.toml files");
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
        println!("Updating install script");
        Ok(())
    }

    /// Update `docker.mdx` and `kubernetes.mdx` with the release version.
    /// Update the kubernetes section of the docs:
    ///   - go to the `helm/chart/router` folder
    ///   - run
    ///   ```helm template --set router.configuration.telemetry.metrics.prometheus.enabled=true  --set managedFederation.apiKey="REDACTED" --set managedFederation.graphRef="REDACTED" --debug .```
    ///   - Paste the output in the `Kubernetes Configuration` example of the `docs/source/containerization/kubernetes.mdx` file
    fn update_docs(&self) -> Result<()> {
        println!("Updating docs");
        Ok(())
    }

    /// Update `helm/chart/router/README.md` by running this from the repo root: `(cd helm/chart && helm-docs router)`.
    ///   (If not installed, you should [install `helm-docs`](https://github.com/norwoodj/helm-docs))
    fn update_helm_charts(&self) -> Result<()> {
        println!("Updating helm chars");
        Ok(())
    }
    /// Update the `image` of the Docker image within `docker-compose*.yml` files inside the `dockerfiles` directory.
    fn docker_files(&self) -> Result<()> {
        println!("Updating docker files");
        Ok(())
    }

    /// Add a new section in `CHANGELOG.md` with the contents of `NEXT_CHANGELOG.md`
    /// Put a Release date and the version number on the new `CHANGELOG.md` section
    /// Update the version in `NEXT_CHANGELOG.md`.
    /// Clear `NEXT_CHANGELOG.md` leaving only the template.
    fn finalize_changelog(&self) -> Result<()> {
        println!("Finalizing changelog");
        Ok(())
    }
    /// Update the license list with `cargo about generate --workspace -o licenses.html about.hbs`.
    ///     (If not installed, you can install `cargo-about` by running `cargo install cargo-about`.)
    /// Run `cargo xtask check-compliance`.
    fn check_compliance(&self) -> Result<()> {
        println!("Checking compliance");
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
        println!("Updating lock file");
        cargo!(["check"]);
        Ok(())
    }

    /// Create the release PR
    fn create_release_pr(&self) -> Result<()> {
        println!("Creating release PR");
        Ok(())
    }
}
