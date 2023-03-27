use anyhow::Result;
use xtask::*;

#[derive(Debug, clap::Parser)]
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
