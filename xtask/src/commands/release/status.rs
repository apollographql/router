use anyhow::Result;
use serde::Serialize;

use super::common::ReleaseCommonArgs;
use super::state;
use super::state::LineState;
use super::state::PrSummary;
use super::state::State;
use super::state::VersionWork;

/// Read-only snapshot of in-flight release work, grouped by release line.
#[derive(Debug, clap::Parser)]
pub struct Status {
    #[command(flatten)]
    pub common: ReleaseCommonArgs,

    /// Emit machine-readable JSON instead of the default human-readable
    /// layout.
    #[clap(long)]
    pub json: bool,

    /// Skip the `git fetch --tags --prune` that normally runs first.  Use
    /// when you're explicitly OK with potentially stale local state (or
    /// have already fetched recently).
    #[clap(long = "no-fetch")]
    pub no_fetch: bool,
}

impl Status {
    pub fn run(&self) -> Result<()> {
        if !self.no_fetch {
            state::fetch(&self.common.origin)?;
        }

        let full_state = state::detect_state(&self.common.repo, &self.common.origin)?;

        let filtered = match &self.common.line {
            Some(line) => State {
                lines: full_state
                    .lines
                    .into_iter()
                    .filter(|ls| ls.line.line == *line)
                    .collect(),
                unpaired_branches: full_state.unpaired_branches,
            },
            None => full_state,
        };

        if self.json {
            print_json(&filtered, &self.common.repo, &self.common.origin)?;
        } else {
            print_human(
                &filtered,
                &self.common.repo,
                &self.common.origin,
                self.no_fetch,
            );
        }

        Ok(())
    }
}

fn print_human(state: &State, repo: &str, origin: &str, no_fetch: bool) {
    println!("Release state — {repo} ({origin})");
    if no_fetch {
        println!("  (local state — did not `git fetch`; pass without --no-fetch for fresh data)");
    }
    println!();

    if state.lines.is_empty() {
        println!("  (no release lines detected)");
    }

    for line_state in &state.lines {
        print_line_human(line_state);
    }

    if !state.unpaired_branches.is_empty() {
        println!();
        println!("  warning: unpaired branches (missing main or dev counterpart):");
        for branch in &state.unpaired_branches {
            println!("    - {branch}");
        }
    }
}

fn print_line_human(ls: &LineState) {
    let line_id = ls.line.line.id();
    let branches = format!(
        "{} / {}",
        ls.line.line.main_branch(),
        ls.line.line.dev_branch()
    );
    let tag = ls
        .latest_release
        .as_ref()
        .map(|v| format!("v{v}"))
        .unwrap_or_else(|| "—".to_string());

    let marker = if ls.line.line.is_lts() {
        "   LTS"
    } else if ls.line.line.is_staging() {
        "   staging"
    } else {
        ""
    };

    println!("  {line_id} ({branches}){marker}");
    println!("    latest released: {tag}");

    if ls.in_progress.is_empty() {
        println!("    in progress: —");
    } else {
        println!("    in progress:");
        for vw in &ls.in_progress {
            print_version_work_human(vw);
        }
    }
    println!();
}

fn print_version_work_human(vw: &VersionWork) {
    let version = format!("v{}", vw.version);
    let mut parts: Vec<String> = Vec::new();

    if let Some(pr) = &vw.release_pr {
        parts.push(format!(
            "release PR #{} ({})",
            pr.number,
            pr_state_label(pr)
        ));
    }
    if let Some(pr) = &vw.prep_pr {
        parts.push(format!("prep PR #{}", pr.number));
    }
    if let Some(pr) = &vw.reconcile_pr {
        parts.push(format!("reconcile PR #{}", pr.number));
    }
    if let Some(pre) = &vw.latest_prerelease {
        parts.push(format!("last pre-release v{pre}"));
    }

    println!("      {version}   {}", parts.join("   "));
}

fn pr_state_label(pr: &PrSummary) -> String {
    if pr.is_draft {
        return "draft".to_string();
    }
    match pr.merge_state_status.as_deref() {
        Some("CLEAN") => "ready, CI green".to_string(),
        Some("BLOCKED") => "ready, blocked".to_string(),
        Some("BEHIND") => "ready, behind base".to_string(),
        Some("DIRTY") => "ready, merge conflicts".to_string(),
        Some("UNSTABLE") => "ready, CI unstable".to_string(),
        Some("UNKNOWN") | None => "ready".to_string(),
        Some(other) => format!("ready, {}", other.to_lowercase()),
    }
}

#[derive(Serialize)]
struct JsonOutput<'a> {
    repo: &'a str,
    origin: &'a str,
    lines: Vec<JsonLine<'a>>,
    unpaired_branches: &'a [String],
}

#[derive(Serialize)]
struct JsonLine<'a> {
    id: String,
    main_branch: String,
    dev_branch: String,
    is_lts: bool,
    is_staging: bool,
    latest_release: Option<String>,
    latest_prerelease: Option<String>,
    in_progress: Vec<JsonVersionWork<'a>>,
}

#[derive(Serialize)]
struct JsonVersionWork<'a> {
    version: String,
    release_pr: Option<&'a PrSummary>,
    prep_pr: Option<&'a PrSummary>,
    reconcile_pr: Option<&'a PrSummary>,
    latest_prerelease: Option<String>,
}

fn print_json(state: &State, repo: &str, origin: &str) -> Result<()> {
    let lines = state
        .lines
        .iter()
        .map(|ls| JsonLine {
            id: ls.line.line.id(),
            main_branch: ls.line.line.main_branch(),
            dev_branch: ls.line.line.dev_branch(),
            is_lts: ls.line.line.is_lts(),
            is_staging: ls.line.line.is_staging(),
            latest_release: ls.latest_release.as_ref().map(|v| v.to_string()),
            latest_prerelease: ls.latest_prerelease.as_ref().map(|v| v.to_string()),
            in_progress: ls
                .in_progress
                .iter()
                .map(|vw| JsonVersionWork {
                    version: vw.version.to_string(),
                    release_pr: vw.release_pr.as_ref(),
                    prep_pr: vw.prep_pr.as_ref(),
                    reconcile_pr: vw.reconcile_pr.as_ref(),
                    latest_prerelease: vw.latest_prerelease.as_ref().map(|v| v.to_string()),
                })
                .collect(),
        })
        .collect();

    let out = JsonOutput {
        repo,
        origin,
        lines,
        unpaired_branches: &state.unpaired_branches,
    };

    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
