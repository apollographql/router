use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Dist {}

impl Dist {
    pub fn run(&self) -> Result<()> {
        cargo!(["build", "--release"]);

        let bin_path = TARGET_DIR.join("release").join(RELEASE_BIN);

        eprintln!("successfully compiled to: {}", &bin_path);

        Ok(())
    }
}
