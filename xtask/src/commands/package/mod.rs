#[cfg(target_os = "macos")]
mod macos;

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8PathBuf;
use structopt::StructOpt;
use xtask::*;

const INCLUDE: &[&str] = &["README.md", "LICENSE", "licenses.html"];
pub(crate) const TARGET_X86_64_MUSL_LINUX: &str = "x86_64-unknown-linux-musl";
pub(crate) const TARGET_X86_64_GNU_LINUX: &str = "x86_64-unknown-linux-gnu";
pub(crate) const TARGET_AARCH64_GNU_LINUX: &str = "aarch64-unknown-linux-gnu";
pub(crate) const TARGET_X86_64_WINDOWS: &str = "x86_64-pc-windows-msvc";
pub(crate) const TARGET_X86_64_MACOS: &str = "x86_64-apple-darwin";
pub(crate) const POSSIBLE_TARGETS: [&str; 5] = [
    TARGET_X86_64_MUSL_LINUX,
    TARGET_X86_64_GNU_LINUX,
    TARGET_AARCH64_GNU_LINUX,
    TARGET_X86_64_WINDOWS,
    TARGET_X86_64_MACOS,
];

#[derive(Debug, StructOpt)]
pub struct Package {
    /// Output tarball.
    #[structopt(long)]
    output: Utf8PathBuf,

    #[cfg(target_os = "macos")]
    #[structopt(flatten)]
    macos: macos::PackageMacos,

    #[structopt(long, default_value, possible_values = &POSSIBLE_TARGETS)]
    target: Target,
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
            self.output
                .join(format!("router-v{}-{}.tar.gz", *PKG_VERSION, self.target))
        } else {
            self.output.to_owned()
        };
        eprintln!("Creating tarball: {output_path}");
        let mut file = flate2::write::GzEncoder::new(
            std::io::BufWriter::new(
                std::fs::File::create(&output_path).context("could not create TGZ file")?,
            ),
            flate2::Compression::default(),
        );
        let mut ar = tar::Builder::new(&mut file);
        eprintln!("Adding {release_path}...");
        ar.append_file(
            Path::new("dist").join(RELEASE_BIN),
            &mut std::fs::File::open(release_path).context("could not open binary")?,
        )
        .context("could not add file to TGZ archive")?;

        for path in INCLUDE {
            eprintln!("Adding {path}...");
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

#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Target {
    MuslLinux,
    GnuLinux,
    ArmLinux,
    Windows,
    MacOS,
    Other,
}

impl Default for Target {
    fn default() -> Self {
        if cfg!(target_arch = "x86_64") {
            if cfg!(target_os = "windows") {
                Target::Windows
            } else if cfg!(target_os = "linux") {
                if cfg!(target_env = "gnu") {
                    Target::GnuLinux
                } else if cfg!(target_env = "musl") {
                    Target::MuslLinux
                } else {
                    Target::Other
                }
            } else if cfg!(target_os = "macos") {
                Target::MacOS
            } else {
                Target::Other
            }
        } else if cfg!(target_arch = "aarch64")
            && cfg!(target_os = "linux")
            && cfg!(target_env = "gnu")
        {
            Target::ArmLinux
        } else {
            Target::Other
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            TARGET_X86_64_MUSL_LINUX => Ok(Self::MuslLinux),
            TARGET_X86_64_GNU_LINUX => Ok(Self::GnuLinux),
            TARGET_AARCH64_GNU_LINUX => Ok(Self::ArmLinux),
            TARGET_X86_64_WINDOWS => Ok(Self::Windows),
            TARGET_X86_64_MACOS => Ok(Self::MacOS),
            _ => Ok(Self::Other),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match &self {
            Target::MuslLinux => TARGET_X86_64_MUSL_LINUX,
            Target::GnuLinux => TARGET_X86_64_GNU_LINUX,
            Target::ArmLinux => TARGET_AARCH64_GNU_LINUX,
            Target::Windows => TARGET_X86_64_WINDOWS,
            Target::MacOS => TARGET_X86_64_MACOS,
            Target::Other => "unknown-target",
        };
        write!(f, "{msg}")
    }
}
