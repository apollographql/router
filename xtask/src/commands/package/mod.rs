#[cfg(target_os = "macos")]
mod macos;

use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use xtask::*;

const INCLUDE: &[&str] = &["README.md", "LICENSE", "licenses.html"];
const RELEASE_DIST_DIR: &str = "release-dist";

pub(crate) const TARGET_X86_64_MUSL_LINUX: &str = "x86_64-unknown-linux-musl";
pub(crate) const TARGET_X86_64_GNU_LINUX: &str = "x86_64-unknown-linux-gnu";
pub(crate) const TARGET_AARCH64_GNU_LINUX: &str = "aarch64-unknown-linux-gnu";
pub(crate) const TARGET_X86_64_WINDOWS: &str = "x86_64-pc-windows-msvc";
pub(crate) const TARGET_X86_64_MACOS: &str = "x86_64-apple-darwin";
pub(crate) const TARGET_ARM64_MACOS: &str = "aarch64-apple-darwin";

#[derive(Debug, clap::Parser)]
pub struct Package {
    /// Output tarball.
    #[clap(long)]
    output: Utf8PathBuf,

    #[cfg(target_os = "macos")]
    #[clap(flatten)]
    macos: macos::PackageMacos,

    #[clap(long)]
    target: Option<Target>,
}

impl Package {
    pub fn run(&self) -> Result<()> {
        let target = self.target.clone().unwrap_or_default();
        let release_path = match &self.target {
            None => TARGET_DIR.join(RELEASE_DIST_DIR).join(RELEASE_BIN),
            Some(target) => TARGET_DIR
                .join(target.to_string())
                .join(RELEASE_DIST_DIR)
                .join(RELEASE_BIN),
        };

        ensure!(
            release_path.exists(),
            "Could not find binary at: {}",
            release_path
        );

        #[cfg(target_os = "macos")]
        self.macos.run(&release_path)?;

        let (output_path, output_is_dir) = self.output_paths()?;

        if target.is_linux_gnu() {
            // Unstripped binary goes in the -debug tarball; a stripped copy goes
            // in the normal release tarball used by production images.
            let debug_output = if output_is_dir {
                self.output
                    .join(format!("router-v{}-{}-debug.tar.gz", *PKG_VERSION, target))
            } else {
                // When a concrete file path is given, derive the debug sibling name.
                let file_name = output_path
                    .file_name()
                    .context("output path has no file name")?;
                let debug_name = if let Some(stem) = file_name.strip_suffix(".tar.gz") {
                    format!("{stem}-debug.tar.gz")
                } else {
                    format!("{file_name}-debug")
                };
                match output_path.parent() {
                    Some(parent) => parent.join(debug_name),
                    None => Utf8PathBuf::from(debug_name),
                }
            };

            create_tarball(&debug_output, &release_path)?;

            let stripped_path = strip_binary(&release_path)?;
            create_tarball(&output_path, &stripped_path)?;
        } else {
            create_tarball(&output_path, &release_path)?;
        }

        Ok(())
    }

    /// Resolve the normal (stripped) output path and whether `--output` was a directory.
    fn output_paths(&self) -> Result<(Utf8PathBuf, bool)> {
        let target = self.target.clone().unwrap_or_default();
        if !self.output.exists() {
            if let Some(path) = self.output.parent() {
                let _ = fs::create_dir_all(path);
            }
            Ok((self.output.to_owned(), false))
        } else if self.output.is_dir() {
            Ok((
                self.output
                    .join(format!("router-v{}-{}.tar.gz", *PKG_VERSION, target)),
                true,
            ))
        } else {
            Ok((self.output.to_owned(), false))
        }
    }
}

fn create_tarball(output_path: &Utf8Path, binary_path: &Utf8Path) -> Result<()> {
    eprintln!("Creating tarball: {output_path}");
    let mut file = flate2::write::GzEncoder::new(
        std::io::BufWriter::new(
            fs::File::create(output_path).context("could not create TGZ file")?,
        ),
        flate2::Compression::default(),
    );
    let mut ar = tar::Builder::new(&mut file);
    eprintln!("Adding {binary_path}...");
    ar.append_file(
        Path::new("dist").join(RELEASE_BIN),
        &mut fs::File::open(binary_path).context("could not open binary")?,
    )
    .context("could not add file to TGZ archive")?;

    for path in INCLUDE {
        eprintln!("Adding {path}...");
        ar.append_file(
            Path::new("dist").join(path),
            &mut fs::File::open(PKG_PROJECT_ROOT.join(path))
                .context("could not open included file")?,
        )
        .context("could not add file to TGZ archive")?;
    }

    ar.finish().context("could not finish TGZ archive")?;
    Ok(())
}

/// Copy `binary_path` and strip debuginfo from the copy, leaving the original intact.
fn strip_binary(binary_path: &Utf8Path) -> Result<Utf8PathBuf> {
    let persist_dir = TARGET_DIR.join("xtask-strip");
    fs::create_dir_all(&persist_dir).context("could not create xtask-strip dir")?;
    let stripped_path = persist_dir.join(RELEASE_BIN);
    fs::copy(binary_path, &stripped_path).context("could not copy binary for stripping")?;

    let strip = which::which("strip").context("`strip` not found on PATH")?;
    let status = Command::new(strip)
        .arg("--strip-debug")
        .arg(stripped_path.as_str())
        .status()
        .context("failed to run strip")?;
    if !status.success() {
        bail!("strip failed with status: {status}");
    }

    Ok(stripped_path)
}

impl Target {
    fn is_linux_gnu(&self) -> bool {
        matches!(self, Target::GnuLinux | Target::ArmLinux)
    }
}

#[derive(Debug, PartialEq, Clone, clap::ValueEnum)]
pub(crate) enum Target {
    #[value(name = "x86_64-unknown-linux-musl")]
    MuslLinux,
    #[value(name = "x86_64-unknown-linux-gnu")]
    GnuLinux,
    #[value(name = "aarch64-unknown-linux-gnu")]
    ArmLinux,
    #[value(name = "x86_64-pc-windows-msvc")]
    Windows,
    #[value(name = "x86_64-apple-darwin")]
    MacOS,
    #[value(name = "aarch64-apple-darwin")]
    ArmMacOS,
    #[value(skip)]
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
        } else if cfg!(target_arch = "aarch64") {
            if cfg!(target_os = "linux") || cfg!(target_env = "gnu") {
                Target::ArmLinux
            } else if cfg!(target_os = "macos") {
                Target::ArmMacOS
            } else {
                Target::Other
            }
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
            TARGET_ARM64_MACOS => Ok(Self::ArmMacOS),
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
            Target::ArmMacOS => TARGET_ARM64_MACOS,
            Target::Other => "unknown-target",
        };
        write!(f, "{msg}")
    }
}
