use anyhow::Result;
use structopt::StructOpt;
use xtask::*;

#[derive(Debug, StructOpt)]
pub struct Licenses {}

impl Licenses {
    pub fn run(&self) -> Result<()> {
        eprintln!("Updating licenses.html...");

        cargo!([
            "about",
            "-L",
            "error",
            "generate",
            "--workspace",
            "-o",
            "licenses.html",
            "about.hbs",
        ]);

        Ok(())
    }
}
