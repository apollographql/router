#[cfg(target_os = "macos")]
mod macos;

use anyhow::{ensure, Context, Result};
use camino::Utf8PathBuf;
use std::path::Path;
use structopt::StructOpt;
use xtask::*;

const INCLUDE: &[&str] = &["README.md", "LICENSE", "licenses.html"];

#[derive(Debug, StructOpt)]
pub struct Package {
    /// Output tarball.
    #[structopt(long)]
    output: Utf8PathBuf,

    #[cfg(target_os = "macos")]
    #[structopt(flatten)]
    macos: macos::PackageMacos,
}

impl Package {
    pub fn run(&self) -> Result<()> {
        let release_path = TARGET_DIR.join("release").join(RELEASE_BIN);

        ensure!(
            release_path.exists(),
            "Could not find binary at: {}",
            release_path
        );

        #[cfg(target_os = "macos")]
        self.macos.run(&release_path)?;

        let output_path = if !self.output.exists() {
            if let Some(path) = self.output.parent() {
                let _ = std::fs::create_dir_all(path);
            }
            self.output.to_owned()
        } else if self.output.is_dir() {
            self.output.join(format!(
                "router-{}-{}-{}.tar.gz",
                *PKG_VERSION,
                // NOTE: same as xtask
                TARGET_ARCH,
                TARGET_OS,
            ))
        } else {
            self.output.to_owned()
        };
        eprintln!("Creating tarball: {}", output_path);
        let mut file = flate2::write::GzEncoder::new(
            std::io::BufWriter::new(
                std::fs::File::create(&output_path).context("could not create TGZ file")?,
            ),
            flate2::Compression::default(),
        );
        let mut ar = tar::Builder::new(&mut file);
        eprintln!("Adding {}...", release_path);
        ar.append_file(
            Path::new("dist").join(RELEASE_BIN),
            &mut std::fs::File::open(release_path).context("could not open binary")?,
        )
        .context("could not add file to TGZ archive")?;

        for path in INCLUDE {
            eprintln!("Adding {}...", path);
            ar.append_file(
                Path::new("dist").join(path),
                &mut std::fs::File::open(PKG_PROJECT_ROOT.join(path))
                    .context("could not open binary")?,
            )
            .context("could not add file to TGZ archive")?;
        }

        ar.finish().context("could not finish TGZ archive")?;

        Ok(())
    }
}

#[cfg(target_arch = "aarch64")]
pub const TARGET_ARCH: &str = "x86_64";

#[cfg(target_arch = "arm")]
pub const TARGET_ARCH: &str = "arm";

#[cfg(target_arch = "x86")]
pub const TARGET_ARCH: &str = "x86";

#[cfg(target_arch = "x86_64")]
pub const TARGET_ARCH: &str = "x86_64";

#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "x86",
    target_arch = "x86_64"
)))]
pub const TARGET_ARCH: &str = "unknown";

#[cfg(target_os = "linux")]
pub const TARGET_OS: &str = "linux";

#[cfg(target_os = "macos")]
pub const TARGET_OS: &str = "macos";

#[cfg(target_os = "windows")]
pub const TARGET_OS: &str = "windows";

#[cfg(target_os = "freebsd")]
pub const TARGET_OS: &str = "freebsd";

#[cfg(not(any(
    target_os = "freebsd",
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
)))]
pub const TARGET_OS: &str = "unknown";
