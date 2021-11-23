use anyhow::{ensure, Result};
use ring::digest::Digest;
use structopt::StructOpt;
use xtask::*;

static LICENSES_HTML_PATH: &str = "licenses.html";

#[derive(Debug, StructOpt)]
pub struct Compliance {}

impl Compliance {
    pub fn run(&self) -> Result<()> {
        cargo!(["deny", "-L", "error", "check"]);

        eprintln!("Checking generated licenses.html file...");

        let licenses_html_before = Self::digest_for_license_file();

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

        let licences_html_after = Self::digest_for_license_file();

        ensure!(
            licenses_html_before.as_ref() == licences_html_after.as_ref(),
            r#"🚨 licenses.html file is not up to date. 🚨\n\
            Please run `cargo about generate --workspace -o licenses.html about.hbs` to generate an up to date licenses list, and check the file in to the repository."#
        );
        Ok(())
    }

    fn digest_for_license_file() -> Digest {
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);

        ctx.update(
            std::fs::read(LICENSES_HTML_PATH)
                .expect("couldn't read file contents")
                .as_slice(),
        );

        ctx.finish()
    }
}
