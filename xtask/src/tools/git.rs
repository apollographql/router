use std::convert::TryFrom;

use crate::tools::Runner;

use anyhow::{Context, Result};
use assert_fs::TempDir;
use camino::Utf8PathBuf;

pub(crate) struct GitRunner {
    temp_dir_path: Utf8PathBuf,
    runner: Runner,

    // we store _temp_dir here since its Drop implementation deletes the directory
    _temp_dir: TempDir,
}

impl GitRunner {
    pub(crate) fn new(verbose: bool) -> Result<Self> {
        let runner = Runner::new("git", verbose)?;
        let temp_dir = TempDir::new().with_context(|| "Could not create temp directory")?;
        let temp_dir_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf())
            .with_context(|| "Temp directory was not valid Utf-8")?;

        Ok(GitRunner {
            runner,
            temp_dir_path,
            _temp_dir: temp_dir,
        })
    }

    pub(crate) fn checkout_router_version(&self, router_version: &str) -> Result<Utf8PathBuf> {
        let repo_name = "router";
        let repo_url = format!("https://github.com/apollographql/{}", repo_name);
        self.runner
            .exec(&["clone", &repo_url], &self.temp_dir_path, None)?;

        let repo_path = self.temp_dir_path.join(repo_name);

        self.runner.exec(
            &["checkout", &format!("tags/{}", router_version)],
            &repo_path,
            None,
        )?;

        Ok(repo_path)
    }
}
