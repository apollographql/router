use anyhow::Context;
use camino::Utf8PathBuf;

use crate::commands::version::RouterVersion;
use crate::target::Target;
use crate::tools::{GitRunner, Runner};
use crate::utils::{self, PKG_PROJECT_ROOT};
use crate::Result;

use std::collections::HashMap;
use std::convert::TryInto;
use std::fs;

pub const FEATURE_SETS: &[&[&str]] = &[
    &[],
    &["otlp-http"],
    &["otlp-tonic"],
    &["otlp-tonic", "tls"],
    &["otlp-grpcio"],
];

pub(crate) struct CargoRunner {
    cargo_package_directory: Utf8PathBuf,
    runner: Runner,
    env: HashMap<String, String>,
    git_runner: Option<GitRunner>,
}

impl CargoRunner {
    pub(crate) fn new(verbose: bool) -> Result<Self> {
        let runner = Runner::new("cargo", verbose)?;
        let cargo_package_directory = PKG_PROJECT_ROOT.clone();

        Ok(CargoRunner {
            cargo_package_directory,
            runner,
            env: HashMap::new(),
            git_runner: None,
        })
    }

    pub(crate) fn set_path(&mut self, cargo_package_directory: Utf8PathBuf) {
        self.cargo_package_directory = cargo_package_directory;
    }

    pub(crate) fn build(
        &mut self,
        target: &Target,
        release: bool,
        version: Option<&RouterVersion>,
    ) -> Result<Utf8PathBuf> {
        if let Some(version) = version {
            let git_runner = GitRunner::new(self.runner.verbose)?;
            let repo_path = git_runner.checkout_router_version(version.to_string().as_str())?;
            self.set_path(repo_path);
            self.git_runner = Some(git_runner);
        }

        let args = vec!["build"];
        self.cargo_exec_with_target(target, args, vec![], release)?;
        let bin_path = self.get_bin_path(target, release)?;
        utils::info(&format!("successfully compiled to `{}`", &bin_path));
        Ok(bin_path)
    }

    pub(crate) fn lint(&mut self) -> Result<()> {
        self.cargo_exec_without_target(vec!["fmt", "--all"], vec!["--check"])?;
        let args = vec!["clippy", "--all"];
        self.cargo_exec_without_target(args, vec!["-D", "warnings"])?;
        Ok(())
    }

    pub(crate) fn test(&mut self, target: &Target) -> Result<()> {
        let args = vec!["test", "--workspace", "--locked", "--no-default-features"];

        for features in FEATURE_SETS {
            if cfg!(windows) && features.contains(&"otlp-grpcio") {
                // TODO: I couldn't make it build on Windows but it is supposed to build.
                continue;
            }

            let mut args = args.clone();
            for feature in features.iter() {
                args.extend(&["--features", feature]);
            }

            self.cargo_exec_with_target(target, args, vec![], false)?;
        }

        Ok(())
    }

    pub(crate) fn get_bin_path(&self, target: &Target, release: bool) -> Result<Utf8PathBuf> {
        let mut out_path = self.cargo_package_directory.clone();
        let mut root_path = PKG_PROJECT_ROOT.clone();

        out_path.push("target");
        root_path.push("target");

        if !target.is_other() {
            out_path.push(target.to_string());
            root_path.push(target.to_string());
        }
        if release {
            out_path.push("release");
            root_path.push("release");
        } else {
            out_path.push("debug");
            root_path.push("debug");
        }

        if out_path != root_path {
            utils::info(&format!(
                "copying contents of `{}` to `{}`",
                &out_path, &root_path
            ));
            copy_dir_all(&out_path, &root_path)
                .with_context(|| "Could not copy build contents to local target directory")?;
        }

        root_path.push("router");

        Ok(root_path)
    }

    fn _cargo_exec(&mut self, cargo_args: Vec<&str>, extra_args: Vec<&str>) -> Result<()> {
        let mut args = cargo_args;
        if !extra_args.is_empty() {
            args.push("--");
            for extra_arg in extra_args {
                args.push(extra_arg);
            }
        }
        let env = if self.env.is_empty() {
            None
        } else {
            Some(&self.env)
        };
        self.runner.exec(&args, &self.cargo_package_directory, env)
    }

    fn cargo_exec_without_target(
        &mut self,
        cargo_args: Vec<&str>,
        extra_args: Vec<&str>,
    ) -> Result<()> {
        self._cargo_exec(cargo_args, extra_args)
    }

    fn cargo_exec_with_target(
        &mut self,
        target: &Target,
        cargo_args: Vec<&str>,
        extra_args: Vec<&str>,
        release: bool,
    ) -> Result<()> {
        let target_args = target.get_args();
        let mut cargo_args = cargo_args;
        cargo_args.extend(
            target_args
                .iter()
                .map(|target_arg| target_arg.as_str())
                .collect::<Vec<_>>(),
        );
        if release {
            cargo_args.push("--release");
        }
        if let Some(env) = target.get_env()? {
            self.env.extend(env);
        }
        self._cargo_exec(cargo_args, extra_args)
    }
}

fn copy_dir_all(source: &Utf8PathBuf, destination: &Utf8PathBuf) -> Result<()> {
    fs::create_dir_all(&destination)?;
    for entry in fs::read_dir(&source)?.flatten() {
        if let Ok(file_type) = entry.file_type() {
            if let Some(file_name) = entry.file_name().to_str() {
                let this_destination = destination.join(file_name);
                let this_source = entry.path().try_into()?;
                if file_type.is_dir() {
                    copy_dir_all(&this_source, &this_destination)?;
                } else {
                    fs::copy(this_source, this_destination)?;
                }
            }
        }
    }
    Ok(())
}
