use std::fs::File;
use std::io;
use std::process::Command;

use anyhow::anyhow;
use anyhow::Result;
use sha2::Digest;
use sha2::Sha256;
use structopt::StructOpt;
use xtask::*;

static LICENSES_HTML_PATH: &str = "licenses.html";

#[derive(Debug, StructOpt)]
pub struct Licenses {}

impl Licenses {
    pub fn run(&self) -> Result<()> {
        eprintln!("Checking generated licenses.html file...");
        let licenses_html_before = Self::digest_for_license_file()?;

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

        let licences_html_after = Self::digest_for_license_file()?;

        (licenses_html_before != licences_html_after).then(|| {
            eprintln!(
                "💅 licenses.html is now up to date. 💅\n\
                Commit the changes and you should be good to go!"
            );
        });

        Ok(())
    }

    fn digest_for_license_file() -> Result<Vec<u8>> {
        let mut digest = Sha256::default();
        io::copy(&mut File::open(LICENSES_HTML_PATH)?, &mut digest)?;
        Ok(digest.finalize().to_vec())
    }
}
