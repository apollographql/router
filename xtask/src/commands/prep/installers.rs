use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use regex::bytes::Regex;

use std::{fs, str};

use crate::utils::{self, PKG_PROJECT_ROOT, PKG_VERSION};

/// prepares our curl/iwr installers
/// with the Cargo.toml version
pub(crate) fn update_versions() -> Result<()> {
    utils::info("updating shell installer versions.");
    let scripts_dir = get_binstall_scripts_root()?;
    update_nix_installer_version(&scripts_dir)?;
    update_win_installer_version(&scripts_dir)
}

/// updates our curl installer with the Cargo.toml version
fn update_nix_installer_version(parent: &Utf8Path) -> Result<()> {
    utils::info("updating nix installer version.");
    let installer = Utf8PathBuf::from(parent).join("nix").join("install.sh");
    let old_installer_contents = fs::read_to_string(installer.as_path())
        .context("Could not read contents of nix installer to a String")?;
    let version_regex = Regex::new(r#"(?:PACKAGE_VERSION="v){1}(.*)"{1}"#)
        .context("Could not create regular expression for nix installer version replacer")?;
    let old_version = str::from_utf8(
        version_regex
            .captures(old_installer_contents.as_bytes())
            .ok_or_else(|| anyhow!("Could not find PACKAGE_VERSION in nix/install.sh"))?
            .get(1)
            .ok_or_else(|| anyhow!("Could not find the version capture group in nix/install.sh"))?
            .as_bytes(),
    )
    .context("Capture group is not valid UTF-8")?;
    let new_installer_contents = old_installer_contents.replace(old_version, &PKG_VERSION);
    fs::write(installer.as_path(), &new_installer_contents)
        .context("Could not write updated PACKAGE_VERSION to nix/install.sh")?;
    Ok(())
}

/// updates our windows installer with the Cargo.toml version
fn update_win_installer_version(parent: &Utf8Path) -> Result<()> {
    utils::info("updating windows installer version.");
    let installer = Utf8PathBuf::from(parent)
        .join("windows")
        .join("install.ps1");
    let old_installer_contents = fs::read_to_string(installer.as_path())
        .context("Could not read contents of windows installer to a String")?;
    let version_regex = Regex::new(r#"(?:\$package_version = 'v){1}(.*)'{1}"#)
        .context("Could not create regular expression for windows installer version replacer")?;
    let old_version = str::from_utf8(
        version_regex
            .captures(old_installer_contents.as_bytes())
            .ok_or_else(|| anyhow!("Could not find $package_version in windows/install.ps1"))?
            .get(1)
            .ok_or_else(|| {
                anyhow!("Could not find the version capture group in windows/install.ps1")
            })?
            .as_bytes(),
    )
    .context("Capture group is not valid UTF-8")?;
    let new_installer_contents = old_installer_contents.replace(old_version, &PKG_VERSION);
    fs::write(installer.as_path(), &new_installer_contents)
        .context("Could not write updated $package_version to windows/install.ps1")?;
    Ok(())
}

/// gets the parent directory
/// of our nix/windows install scripts
fn get_binstall_scripts_root() -> Result<Utf8PathBuf> {
    Ok(PKG_PROJECT_ROOT
        .join("installers")
        .join("binstall")
        .join("scripts"))
}
