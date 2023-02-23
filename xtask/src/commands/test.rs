use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

const TEST_DEFAULT_ARGS: &[&str] = &[
    "test",
    "--manifest-path",
    "apollo-router/Cargo.toml",
    "--locked",
    "--jobs",
    "4",
    "--",
    "--test-threads",
    "6",
];

#[derive(Debug, StructOpt)]
pub struct Test {}

impl Test {
    pub fn run(&self) -> Result<()> {
        eprintln!("Running tests");
        cargo!(TEST_DEFAULT_ARGS);
        Ok(())
    }
}
