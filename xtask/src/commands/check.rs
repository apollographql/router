use anyhow::{ensure, Result};
use std::fs::read_to_string;
use structopt::StructOpt;
use xtask::*;

static LICENSES_HTML_PATH: &str = "licenses.html";

#[derive(Debug, StructOpt)]
pub struct Compliance {}

impl Compliance {
    pub fn run(&self) -> Result<()> {
        cargo!(["deny", "-L", "error", "check"]);

        eprintln!("checking generated licenses.html file");

        let licenses_html_before = read_to_string(LICENSES_HTML_PATH)?;

        cargo!([
            "about",
            "-L",
            "error",
            "generate",
            "--workspace",
            "-o",
            "licenses.html",
            "about.hbs"
        ]);

        let licences_html_after = read_to_string(LICENSES_HTML_PATH)?;

        ensure!(
            licenses_html_before == licences_html_after,
            r#"🚨 licenses.html file is not up to date. 🚨\n\
            Please run `cargo about generate --workspace -o licenses.html about.hbs` to generate an up to date licenses list, and check the file in to the repository."#
        );
        Ok(())
    }
}
