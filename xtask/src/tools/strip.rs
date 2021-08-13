use anyhow::Result;
use camino::Utf8PathBuf;

use crate::tools::Runner;
use crate::utils::PKG_PROJECT_ROOT;

pub(crate) struct StripRunner {
    runner: Runner,
    router_executable: Utf8PathBuf,
}

impl StripRunner {
    pub(crate) fn new(router_executable: Utf8PathBuf, verbose: bool) -> Result<Self> {
        let runner = Runner::new("strip", verbose)?;
        Ok(StripRunner {
            runner,
            router_executable,
        })
    }

    pub(crate) fn run(&self) -> Result<()> {
        let project_root = PKG_PROJECT_ROOT.clone();
        self.runner
            .exec(&[&self.router_executable.to_string()], &project_root, None)?;
        Ok(())
    }
}
