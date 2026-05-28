//! Detect the current state of release work on the remote.
//!
//! Everything here is derived from two sources:
//!   1. Local git state (branches under `refs/remotes/<origin>/`, tags under `refs/tags/`).
//!   2. The GitHub API via `gh pr list --json ...` (single call, attributed by head branch).
//!
//! Callers decide whether to `git fetch` before calling [`detect_state`].

use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::Result;
use anyhow::anyhow;
use semver::Version;
use serde::Deserialize;
use serde::Serialize;

use super::common::Line;

/// A release line present on the remote, with both its main and dev branches
/// confirmed to exist.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseLine {
    pub line: Line,
}

/// Per-line snapshot of release state.
#[derive(Debug, Clone)]
pub struct LineState {
    pub line: ReleaseLine,
    /// Latest final-release tag on this line (no pre-release suffix).
    pub latest_release: Option<Version>,
    /// Latest pre-release tag on this line.
    pub latest_prerelease: Option<Version>,
    /// Versions with work in progress on this line (release PR, prep PR, or reconcile PR open).
    pub in_progress: Vec<VersionWork>,
}

/// An in-progress release version on some line.
#[derive(Debug, Clone)]
pub struct VersionWork {
    pub version: Version,
    pub release_pr: Option<PrSummary>,
    pub prep_pr: Option<PrSummary>,
    pub reconcile_pr: Option<PrSummary>,
    /// Latest pre-release tag matching this version (e.g., v2.14.0-rc.2 for the 2.14.0 branch).
    pub latest_prerelease: Option<Version>,
}

/// Minimal PR info we care about for status display.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    #[serde(rename = "headRefName")]
    pub head_ref_name: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    #[serde(rename = "mergeStateStatus")]
    pub merge_state_status: Option<String>,
    pub url: Option<String>,
}

/// Complete state snapshot — one entry per detected line.
#[derive(Debug, Clone)]
pub struct State {
    pub lines: Vec<LineState>,
    /// Branches matching a main/dev pattern that didn't have a paired counterpart.
    pub unpaired_branches: Vec<String>,
}

/// Run `git fetch --tags --prune <origin>`.  Call before [`detect_state`] if
/// the caller wants fresh state.
pub fn fetch(origin: &str) -> Result<()> {
    let status = std::process::Command::new(which::which("git")?)
        .args(["fetch", "--tags", "--prune", origin])
        .status()?;
    if !status.success() {
        return Err(anyhow!("git fetch failed"));
    }
    Ok(())
}

/// Detect the current release state.  Reads from local git refs (assuming a
/// recent `fetch`) and the GitHub API via `gh`.
pub fn detect_state(repo: &str, origin: &str) -> Result<State> {
    let remote_branches = list_remote_branches(origin)?;
    let (lines, unpaired_branches) = pair_lines(&remote_branches);
    let version_branches = collect_version_branches(&remote_branches);
    let tags = list_tags()?;
    let prs = list_open_prs(repo)?;

    let line_states = lines
        .iter()
        .map(|rl| build_line_state(rl.clone(), &lines, &version_branches, &tags, &prs))
        .collect();

    Ok(State {
        lines: line_states,
        unpaired_branches,
    })
}

/// List all remote branches (short names, e.g., `main`, `dev-v2.10.x`, `2.15.0`).
///
/// Uses local refs under `refs/remotes/<origin>/`, so call [`fetch`] first
/// if fresh state is needed.
fn list_remote_branches(origin: &str) -> Result<Vec<String>> {
    let refspec = format!("refs/remotes/{origin}/");
    let output = std::process::Command::new(which::which("git")?)
        .args(["for-each-ref", "--format=%(refname:short)", &refspec])
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let prefix = format!("{origin}/");
    let branches = String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix).map(str::to_string))
        .filter(|name| name != "HEAD")
        .collect();
    Ok(branches)
}

/// Group `main`/`dev` branches into paired release lines.  Unpaired halves go
/// into the `unpaired_branches` list so callers can surface a warning.
fn pair_lines(branches: &[String]) -> (Vec<ReleaseLine>, Vec<String>) {
    let mut mains: HashMap<Line, String> = HashMap::new();
    let mut devs: HashMap<Line, String> = HashMap::new();

    for branch in branches {
        if let Some((line, is_main)) = Line::parse_branch(branch) {
            if is_main {
                mains.insert(line, branch.clone());
            } else {
                devs.insert(line, branch.clone());
            }
        }
    }

    let mut lines: Vec<ReleaseLine> = Vec::new();
    let mut unpaired: Vec<String> = Vec::new();

    let all_line_keys: HashSet<Line> = mains.keys().chain(devs.keys()).cloned().collect();

    for line in all_line_keys {
        let has_main = mains.contains_key(&line);
        let has_dev = devs.contains_key(&line);

        // Staging lines are allowed to exist with dev-only (no main) — they
        // haven't shipped a first release yet.
        let staging_solo_ok = matches!(line, Line::Staging { .. }) && has_dev && !has_main;

        if (has_main && has_dev) || staging_solo_ok {
            lines.push(ReleaseLine { line });
        } else if has_main {
            unpaired.push(mains[&line].clone());
        } else {
            unpaired.push(devs[&line].clone());
        }
    }

    // Stable ordering: tip first, then staging lines descending by major, then
    // LTS lines descending by major+minor.
    lines.sort_by_key(|a| line_order_key(&a.line));

    (lines, unpaired)
}

/// Ordering key: lower sorts first.
///   - Tip: (0, 0, 0)
///   - Staging: (1, -major, 0)  — newer majors before older
///   - LTS: (2, -major, -minor) — newer versions before older
fn line_order_key(line: &Line) -> (u8, i64, i64) {
    match line {
        Line::Tip => (0, 0, 0),
        Line::Staging { major } => (1, -(*major as i64), 0),
        Line::Lts { major, minor } => (2, -(*major as i64), -(*minor as i64)),
    }
}

/// Branches whose name parses as `<major>.<minor>.<patch>` — version branches
/// used as release staging branches.
fn collect_version_branches(branches: &[String]) -> Vec<Version> {
    branches
        .iter()
        .filter_map(|name| Version::parse(name).ok())
        // Exclude pre-releases: version branches are plain `2.14.0`, not `2.14.0-rc.2`.
        .filter(|v| v.pre.is_empty())
        .collect()
}

/// Read all tags matching `v<semver>` from local refs.
fn list_tags() -> Result<Vec<Version>> {
    let output = std::process::Command::new(which::which("git")?)
        .args(["tag", "--list", "v*"])
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git tag --list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let tags = String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.strip_prefix('v'))
        .filter_map(|s| Version::parse(s).ok())
        .collect();
    Ok(tags)
}

/// List all open PRs in the target repo via `gh pr list --json ...`.
fn list_open_prs(repo: &str) -> Result<Vec<PrSummary>> {
    if !xtask::gh::available() {
        return Err(anyhow!(
            "the `gh` CLI is required for release state detection (install from https://cli.github.com/ and run `gh auth login`)"
        ));
    }
    let args = [
        "--repo",
        repo,
        "pr",
        "list",
        "--state",
        "open",
        "--limit",
        "200",
        "--json",
        "number,title,headRefName,isDraft,mergeStateStatus,url",
    ];
    let prs: Vec<PrSummary> = xtask::gh::json(args)?;
    Ok(prs)
}

/// Does any LTS line in `all_lines` claim this `major.minor`?
fn any_lts_claims(major: u64, minor: u64, all_lines: &[ReleaseLine]) -> bool {
    all_lines
        .iter()
        .any(|rl| matches!(rl.line, Line::Lts { major: m, minor: n } if m == major && n == minor))
}

/// Does any Staging line in `all_lines` claim this `major`?
fn any_staging_claims(major: u64, all_lines: &[ReleaseLine]) -> bool {
    all_lines
        .iter()
        .any(|rl| matches!(rl.line, Line::Staging { major: m } if m == major))
}

fn build_line_state(
    line: ReleaseLine,
    all_lines: &[ReleaseLine],
    version_branches: &[Version],
    tags: &[Version],
    prs: &[PrSummary],
) -> LineState {
    // Attribution rules:
    //   - LTS line `<maj>.<min>.x`:  claims versions where maj+min match exactly.
    //   - Staging line `<maj>.x`:    claims versions where maj matches AND no LTS line
    //                                claims that specific maj+min.
    //   - Tip:                       claims versions that no Staging or LTS line claims.
    let claims = |v: &Version| -> bool {
        match line.line {
            Line::Lts { major, minor } => v.major == major && v.minor == minor,
            Line::Staging { major } => {
                v.major == major && !any_lts_claims(v.major, v.minor, all_lines)
            }
            Line::Tip => {
                !any_lts_claims(v.major, v.minor, all_lines)
                    && !any_staging_claims(v.major, all_lines)
            }
        }
    };

    let line_tags: Vec<&Version> = tags.iter().filter(|v| claims(v)).collect();

    let latest_release = line_tags
        .iter()
        .filter(|v| v.pre.is_empty())
        .copied()
        .max()
        .cloned();
    let latest_prerelease = line_tags
        .iter()
        .filter(|v| !v.pre.is_empty())
        .copied()
        .max()
        .cloned();

    // Version branches attributed to this line.
    let line_versions: Vec<&Version> = version_branches.iter().filter(|v| claims(v)).collect();

    let in_progress = line_versions
        .into_iter()
        .map(|version| {
            let release_pr = prs
                .iter()
                .find(|pr| pr.head_ref_name == version.to_string())
                .cloned();
            let prep_pr = prs
                .iter()
                .find(|pr| pr.head_ref_name == format!("prep-{version}"))
                .cloned();
            let reconcile_pr = prs
                .iter()
                .find(|pr| pr.head_ref_name == format!("reconcile-v{version}"))
                .cloned();
            let latest_prerelease_for_version = tags
                .iter()
                .filter(|v| {
                    v.major == version.major
                        && v.minor == version.minor
                        && v.patch == version.patch
                        && !v.pre.is_empty()
                })
                .max()
                .cloned();
            VersionWork {
                version: version.clone(),
                release_pr,
                prep_pr,
                reconcile_pr,
                latest_prerelease: latest_prerelease_for_version,
            }
        })
        .filter(|vw| {
            vw.release_pr.is_some()
                || vw.prep_pr.is_some()
                || vw.reconcile_pr.is_some()
                || vw.latest_prerelease.is_some()
        })
        .collect();

    LineState {
        line,
        latest_release,
        latest_prerelease,
        in_progress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn pair_lines_tip_only() {
        let (lines, unpaired) = pair_lines(&[b("main"), b("dev"), b("feature-foo")]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].line, Line::Tip);
        assert!(unpaired.is_empty());
    }

    #[test]
    fn pair_lines_tip_and_lts() {
        let (lines, unpaired) = pair_lines(&[
            b("main"),
            b("dev"),
            b("main-v2.10.x"),
            b("dev-v2.10.x"),
            b("main-v2.13.x"),
            b("dev-v2.13.x"),
        ]);
        assert_eq!(lines.len(), 3);
        // Sorted: tip first, then LTS descending by version.
        assert_eq!(lines[0].line, Line::Tip);
        assert_eq!(
            lines[1].line,
            Line::Lts {
                major: 2,
                minor: 13
            }
        );
        assert_eq!(
            lines[2].line,
            Line::Lts {
                major: 2,
                minor: 10
            }
        );
        assert!(unpaired.is_empty());
    }

    #[test]
    fn pair_lines_unpaired_warning() {
        let (lines, unpaired) = pair_lines(&[b("main"), b("dev"), b("main-v2.9.x")]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].line, Line::Tip);
        assert_eq!(unpaired, vec![b("main-v2.9.x")]);
    }

    #[test]
    fn collect_version_branches_filters_non_semver() {
        let v = collect_version_branches(&[
            b("main"),
            b("2.14.0"),
            b("2.15.0"),
            b("prep-2.14.0"),
            b("reconcile-v2.14.0"),
            b("feature-foo"),
        ]);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], Version::parse("2.14.0").unwrap());
        assert_eq!(v[1], Version::parse("2.15.0").unwrap());
    }
}
