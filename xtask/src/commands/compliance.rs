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
pub struct Compliance {
    /// Do not run the compliance test
    #[structopt(long)]
    skip_compliance: bool,
}

impl Compliance {
    pub fn run(&self) -> Result<()> {
        cargo!(["deny", "-L", "error", "check"]);

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

        if licenses_html_before != licences_html_after {
            eprintln!(
                "{}",
                String::from_utf8_lossy(
                    Command::new(which::which("git")?)
                        .args(["diff", LICENSES_HTML_PATH])
                        .output()?
                        .stdout
                        .as_slice()
                )
            );

            Err(anyhow!("🚨 licenses.html file is not up to date. 🚨\n\
            Please run `cargo about generate --workspace -o licenses.html about.hbs` to generate an up to date licenses list, and check the file in to the repository.\n\
            💡 You can install `cargo-about` by running `cargo install cargo-about`."))
        } else {
            Ok(())
        }
    }

    pub fn run_local(&self) -> Result<()> {
        if !self.skip_compliance {
            eprintln!("Checking generated licenses.html file...");

            cargo!(["deny", "-L", "error", "check"]);

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
        } else {
            Ok(())
        }
    }

    fn digest_for_license_file() -> Result<Vec<u8>> {
        let mut digest = Sha256::default();
        io::copy(&mut File::open(LICENSES_HTML_PATH)?, &mut digest)?;
        Ok(digest.finalize().to_vec())
    }
}
