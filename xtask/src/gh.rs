//! Shared helpers for shelling out to the GitHub CLI (`gh`).
//!
//! Every caller that wants to use `gh` should go through this module rather
//! than re-implementing detection, auth, and subprocess plumbing.

use std::process::Command;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::de::DeserializeOwned;

/// Returns `true` if the `gh` CLI is available on PATH.
pub fn available() -> bool {
    which::which("gh").is_ok()
}

/// Fetch the current `gh` auth token via `gh auth token`.
///
/// Errors if `gh` isn't installed, if `gh auth token` fails, or if the token
/// comes back empty (usually meaning the user hasn't run `gh auth login`).
pub fn token() -> Result<String> {
    let gh =
        which::which("gh").map_err(|_| anyhow!("the `gh` CLI is not installed or not on PATH"))?;

    let output = Command::new(gh)
        .args(["auth", "token"])
        .output()
        .context("failed to run `gh auth token`")?;

    if !output.status.success() {
        return Err(anyhow!(
            "`gh auth token` failed (run `gh auth login`?): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let token = String::from_utf8(output.stdout)
        .context("`gh auth token` output was not valid UTF-8")?
        .trim()
        .to_string();

    if token.is_empty() {
        return Err(anyhow!(
            "`gh auth token` returned an empty token (run `gh auth login`?)"
        ));
    }

    Ok(token)
}

/// Run `gh` with the given args, streaming stdout/stderr to the parent
/// process.  Fails if the exit status is non-zero.
pub fn run<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let gh =
        which::which("gh").map_err(|_| anyhow!("the `gh` CLI is not installed or not on PATH"))?;

    let status = Command::new(gh)
        .args(args)
        .status()
        .context("failed to spawn `gh`")?;

    if !status.success() {
        return Err(anyhow!("`gh` exited with status {}", status));
    }

    Ok(())
}

/// Run `gh` with the given args, capture stdout, and deserialize it as JSON.
///
/// Typically used with `gh pr list --json ...` or similar JSON-emitting
/// invocations.  Fails if the exit status is non-zero, stdout is not valid
/// UTF-8, or the JSON doesn't deserialize into `T`.
pub fn json<T, I, S>(args: I) -> Result<T>
where
    T: DeserializeOwned,
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let gh =
        which::which("gh").map_err(|_| anyhow!("the `gh` CLI is not installed or not on PATH"))?;

    let output = Command::new(gh)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to spawn `gh`")?;

    if !output.status.success() {
        return Err(anyhow!(
            "`gh` exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "failed to parse `gh` JSON output: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}
