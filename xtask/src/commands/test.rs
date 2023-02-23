use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

const TEST_DEFAULT_ARGS: &[&str] = &["test", "--all", "--locked"];

#[derive(Debug, StructOpt)]
pub struct Test {}

impl Test {
    pub fn run(&self) -> Result<()> {
        eprintln!("Running tests");
        cargo!(TEST_DEFAULT_ARGS);

        #[cfg(windows)]
        {
            // dirty hack. Node processes on windows will not shut down cleanly.
            let _ = std::process::Command::new("taskkill")
                .args(["/f", "/im", "node.exe"])
                .spawn();
        }

        Ok(())
    }
}
