use anyhow::Result;
use xtask::*;

#[derive(Debug, clap::Parser)]
pub struct Dist {}

impl Dist {
    pub fn run(&self) -> Result<()> {
        cargo!(["build", "--release"]);

        let bin_path = TARGET_DIR.join("release").join(RELEASE_BIN);

        eprintln!("successfully compiled to: {}", &bin_path);

        Ok(())
    }
}
