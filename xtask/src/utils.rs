use ansi_term::Colour::White;
use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;
use cargo_metadata::MetadataCommand;
use lazy_static::lazy_static;

use std::{convert::TryFrom, env, str};

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
#[cfg(not(windows))]
pub const RELEASE_BIN: &str = "router";
#[cfg(windows)]
pub const RELEASE_BIN: &str = "router.exe";
#[allow(dead_code)]
pub const PKG_PROJECT_NAME: &str = "router";

lazy_static! {
    pub(crate) static ref PKG_VERSION: String =
        router_version().expect("Could not find Router's version.");
    pub(crate) static ref PKG_PROJECT_ROOT: Utf8PathBuf =
        project_root().expect("Could not find Router's project root.");
    pub(crate) static ref TARGET_DIR: Utf8PathBuf =
        target_dir().expect("Could not find Router's target dir.");
}

pub(crate) fn info(msg: &str) {
    let info_prefix = White.bold().paint("info:");
    eprintln!("{} {}", &info_prefix, msg);
}

#[macro_export]
macro_rules! info {
    ($msg:expr $(, $($tokens:tt)* )?) => {{
        let info_prefix = ansi_term::Colour::White.bold().paint("info:");
        eprintln!(concat!("{} ", $msg), &info_prefix $(, $($tokens)*)*);
    }};
}

fn router_version() -> Result<String> {
    let project_root = project_root()?;
    let metadata = MetadataCommand::new()
        .manifest_path(project_root.join("Cargo.toml"))
        .exec()?;

    Ok(metadata
        .root_package()
        .ok_or_else(|| anyhow!("Could not find root package."))?
        .version
        .to_string())
}

fn project_root() -> Result<Utf8PathBuf> {
    let manifest_dir = Utf8PathBuf::try_from(MANIFEST_DIR)
        .with_context(|| "Could not find the root directory.")?;
    let root_dir = manifest_dir
        .ancestors()
        .nth(1)
        .ok_or_else(|| anyhow!("Could not find project root."))?;
    Ok(root_dir.to_path_buf())
}

fn target_dir() -> Result<Utf8PathBuf> {
    let project_root = project_root()?;
    let metadata = MetadataCommand::new()
        .manifest_path(project_root.join("Cargo.toml"))
        .exec()?;

    Ok(metadata.target_directory)
}
