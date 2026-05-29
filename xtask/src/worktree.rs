//! Isolated git-worktree helper for release commands.
//!
//! Release commands that mutate branches, write files, or push refs
//! should NOT do that work in the user's primary checkout — that risks
//! clobbering their working state, racing with other concurrent
//! commands, or leaving them on a weird branch on failure.
//!
//! Instead, create a [`WorkTree`] pointed at the appropriate base ref;
//! all git operations run in that isolated directory.  The Drop impl
//! removes the worktree on success.  Use [`WorkTree::keep`] to leave
//! it in place for post-mortem debugging.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;

pub struct WorkTree {
    path: PathBuf,
    keep_on_drop: bool,
}

impl WorkTree {
    /// Create a new linked worktree at `path`, checked out at `base_ref`
    /// (which can be a branch, tag, or commit SHA).
    ///
    /// `path` is created by `git worktree add`; if it already exists git
    /// will refuse and we surface that error.
    pub fn create(path: PathBuf, base_ref: &str) -> Result<Self> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8: {path:?}"))?;
        let status = Command::new("git")
            .args(["worktree", "add", path_str, base_ref])
            .status()
            .context("failed to spawn `git worktree add`")?;
        if !status.success() {
            return Err(anyhow!("`git worktree add {path_str} {base_ref}` failed"));
        }
        Ok(WorkTree {
            path,
            keep_on_drop: false,
        })
    }

    /// Whether the worktree directory survives Drop.  Set this when the
    /// command fails and you want the user to be able to inspect state.
    pub fn keep(&mut self) {
        self.keep_on_drop = true;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `git <args>` inside the worktree.  Streams stdout/stderr to
    /// the parent process.  Errors if the command's exit status is
    /// non-zero.
    pub fn git<I, S>(&self, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let status = Command::new("git")
            .current_dir(&self.path)
            .args(args)
            .status()
            .context("failed to spawn `git`")?;
        if !status.success() {
            return Err(anyhow!(
                "git command exited {status} in worktree {}",
                self.path.display()
            ));
        }
        Ok(())
    }

    /// Run `git <args>` inside the worktree, capture stdout, return as
    /// a `String` with trailing whitespace trimmed.  Errors on non-zero
    /// exit or non-UTF-8 output.
    pub fn git_out<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new("git")
            .current_dir(&self.path)
            .args(args)
            .output()
            .context("failed to spawn `git`")?;
        if !output.status.success() {
            return Err(anyhow!(
                "git command failed in worktree {}: {}",
                self.path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8(output.stdout)
            .context("git output was not valid UTF-8")?
            .trim_end()
            .to_string())
    }
}

impl Drop for WorkTree {
    fn drop(&mut self) {
        if self.keep_on_drop {
            eprintln!(
                "(keeping worktree at {} for inspection — remove with `git worktree remove --force {0}`)",
                self.path.display()
            );
            return;
        }
        if let Some(path_str) = self.path.to_str() {
            // Best-effort cleanup; ignore failure (we may already be
            // panicking).
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force", path_str])
                .status();
        }
    }
}

/// Compute a default worktree path for a release operation.  Sibling to
/// the project root, so it's visible and not in temp storage.
pub fn default_path(project_root: &Path, slug: &str) -> PathBuf {
    let parent = project_root.parent().unwrap_or(project_root);
    let leaf = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("router");
    parent.join(format!("{leaf}-{slug}"))
}
