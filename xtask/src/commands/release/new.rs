//! `cargo xtask release new` — cut a fresh release branch and open the
//! draft release PR.
//!
//! Per-line behavior:
//!
//! - **Tip patch** (`--line tip --patch`): cut from the line's latest
//!   release tag (NOT from `dev`).  `dev` typically has post-release
//!   work that shouldn't ship in a patch.
//! - **Tip minor / major**: cut from `dev`.
//! - **LTS patch** (`--line 2.10.x --patch`): cut from the line's dev
//!   branch (e.g., `dev-v2.10.x`), since LTS dev IS the locked
//!   maintenance trunk — everything on it is already a backport.
//! - **Staging line** (`--line 3.x --patch` or first release): cut from
//!   `dev-v3.x`.
//!
//! Always adds a `--allow-empty` commit if the cut branch has no diff
//! against the line's main branch (otherwise GitHub refuses to open
//! the PR).

use std::str::FromStr;

use anyhow::Result;
use anyhow::anyhow;
use console::style;
use dialoguer::Confirm;
use dialoguer::Input;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use semver::Version;
use xtask::*;

use super::common::Line;
use super::common::ReleaseCommonArgs;
use super::state;

/// Either a kind of bump (`patch`/`minor`/`major`) or a specific version.
#[derive(Debug, Clone)]
pub enum NewVersion {
    Patch,
    Minor,
    Major,
    Specific(Version),
}

impl FromStr for NewVersion {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "patch" => Ok(NewVersion::Patch),
            "minor" => Ok(NewVersion::Minor),
            "major" => Ok(NewVersion::Major),
            v => {
                let parsed = Version::parse(v).map_err(|e| {
                    anyhow!("invalid version or kind {v:?}: {e}; expected `patch`, `minor`, `major`, or a semver like `2.14.1`")
                })?;
                Ok(NewVersion::Specific(parsed))
            }
        }
    }
}

/// Cut a new release branch and open the draft release PR.
///
/// Operates inside an isolated `git worktree` (sibling to your repo),
/// so your primary checkout's branch and working tree are never touched.
#[derive(Debug, clap::Parser)]
pub struct New {
    #[command(flatten)]
    pub common: ReleaseCommonArgs,

    /// `patch`, `minor`, `major`, or a specific version like `2.14.1`.
    /// If omitted and running interactively, you'll be prompted.
    pub version: Option<NewVersion>,

    /// Override the starting commit-ish.  Defaults differ by line:
    ///   - Tip patch       → latest release tag (e.g., `v2.14.0`)
    ///   - Tip minor/major → `dev`
    ///   - LTS patch       → line's dev branch (e.g., `dev-v2.10.x`)
    ///   - Staging         → line's dev branch
    #[clap(long)]
    pub from: Option<String>,

    /// On failure, keep the isolated worktree directory for debugging
    /// instead of cleaning it up.  Always passed-through behavior on
    /// success (cleans up).
    #[clap(long)]
    pub keep_worktree: bool,

    /// Override the worktree directory location.  Defaults to a sibling
    /// of the project root, named `<project>-release-new-<version>`.
    #[clap(long)]
    pub worktree_path: Option<std::path::PathBuf>,

    /// Print the exact commands without executing.
    #[clap(long)]
    pub dry_run: bool,
}

impl New {
    pub fn run(&self) -> Result<()> {
        // Cheap validations first so a misconfigured invocation errors
        // before we touch the network.
        let line = self.resolve_line()?;
        let kind = self.resolve_kind()?;

        // Now that we know the intent is valid, fetch so latest tags +
        // remote branches are fresh.  Touches `.git` (shared with
        // worktrees) but does NOT mutate the user's working tree or HEAD.
        if !self.dry_run {
            self.fetch()?;
        }

        let target = resolve_target_version(&line, &kind, |l| self.latest_release_on_line(l))?;
        let from = self.resolve_from(&line, &kind)?;

        self.print_plan(&line, &kind, &target, &from);

        if self.dry_run {
            self.print_dry_run_steps(&line, &target, &from);
            return Ok(());
        }

        // Pre-flight: open release PR for this version already?
        self.check_no_existing_release_pr(&target)?;

        if !self.common.non_interactive {
            let proceed = Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(format!(
                    "Cut {} from {} and open draft release PR into {}?",
                    style(format!("v{target}")).cyan().bold(),
                    style(&from).cyan(),
                    style(line.main_branch()).cyan(),
                ))
                .default(true)
                .interact()?;
            if !proceed {
                eprintln!("{}", style("Aborted by user.").yellow());
                return Ok(());
            }
        }

        // All destructive ops happen in an isolated worktree.  Drop cleans up
        // unless we mark `keep` after a failure (or --keep-worktree).
        let wt_path = self.worktree_path.clone().unwrap_or_else(|| {
            xtask::worktree::default_path(
                PKG_PROJECT_ROOT.as_std_path(),
                &format!("release-new-{target}"),
            )
        });
        eprintln!(
            "{} {}",
            style("Creating isolated worktree at").dim(),
            style(wt_path.display()).cyan(),
        );
        let mut wt = xtask::worktree::WorkTree::create(wt_path.clone(), &from)?;
        if self.keep_worktree {
            wt.keep();
        }

        let result = self.execute_in_worktree(&wt, &line, &target);
        if result.is_err() && !self.keep_worktree {
            eprintln!(
                "{}",
                style(format!(
                    "→ keeping worktree at {} for inspection",
                    wt.path().display()
                ))
                .yellow()
            );
            wt.keep();
        }
        result?;

        eprintln!(
            "{}",
            style(format!("Done.  Draft release PR opened for v{target}.")).green()
        );
        Ok(())
    }

    /// Run the destructive steps inside the isolated worktree.  Split out so
    /// that on failure we can choose to keep the worktree for debugging.
    fn execute_in_worktree(
        &self,
        wt: &xtask::worktree::WorkTree,
        line: &Line,
        target: &Version,
    ) -> Result<()> {
        let branch = target.to_string();

        // Worktree was created at `from` — now create the version branch on top.
        wt.git(["checkout", "-b", &branch])?;

        // Empty-commit shim if needed.
        self.maybe_add_empty_commit(wt, line, target)?;

        // Push the new branch.
        wt.git([
            "push",
            "--set-upstream",
            self.common.origin.as_str(),
            &branch,
        ])?;

        // Open the PR.  `gh` doesn't care about cwd when --repo is passed.
        self.open_release_pr(line, target)?;

        Ok(())
    }

    /// Run `git fetch --tags --prune <origin>` from the user's main checkout.
    /// Doesn't change HEAD or working tree — purely updates remote refs.
    fn fetch(&self) -> Result<()> {
        eprintln!(
            "{} {}",
            style("Fetching from").dim(),
            style(&self.common.origin).cyan(),
        );
        let status = std::process::Command::new(which::which("git")?)
            .current_dir(&*PKG_PROJECT_ROOT)
            .args(["fetch", "--tags", "--prune", self.common.origin.as_str()])
            .status()?;
        if !status.success() {
            return Err(anyhow!("git fetch failed"));
        }
        Ok(())
    }

    /// Resolve the line — flag, current branch, or default to tip.
    fn resolve_line(&self) -> Result<Line> {
        if let Some(line) = self.common.resolve_line() {
            return Ok(line);
        }
        if self.common.non_interactive {
            return Err(anyhow!(
                "could not infer release line; pass --line tip or --line <major>.<minor>.x"
            ));
        }
        // Default to tip when interactive but no line specified.  Most common case.
        Ok(Line::Tip)
    }

    /// Resolve the version kind — `--version` arg, current state, or interactive prompt.
    fn resolve_kind(&self) -> Result<NewVersion> {
        if let Some(kind) = &self.version {
            return Ok(kind.clone());
        }
        if self.common.non_interactive {
            return Err(anyhow!(
                "version is required in non-interactive mode; pass `patch`, `minor`, `major`, or a specific version like `2.14.1`"
            ));
        }
        // Interactive: present a Select.
        let items = [
            "Patch  (e.g., bump 2.14.0 → 2.14.1)",
            "Minor  (e.g., bump 2.14.0 → 2.15.0)",
            "Major  (e.g., bump 2.14.0 → 3.0.0)",
            "Specific version...",
        ];
        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("What kind of release?")
            .items(items)
            .default(0)
            .interact()?;
        match selection {
            0 => Ok(NewVersion::Patch),
            1 => Ok(NewVersion::Minor),
            2 => Ok(NewVersion::Major),
            3 => {
                let input: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("Specific version (e.g., 2.14.1)")
                    .interact_text()?;
                NewVersion::from_str(input.trim())
            }
            _ => unreachable!(),
        }
    }

    /// Read the latest released (non-pre-release) tag on the line from local refs.
    /// Assumes `git fetch --tags` has run recently.
    fn latest_release_on_line(&self, line: &Line) -> Result<Option<Version>> {
        let output = std::process::Command::new(which::which("git")?)
            .args(["tag", "--list", "v*"])
            .output()?;
        if !output.status.success() {
            return Err(anyhow!("git tag --list failed"));
        }
        let latest = String::from_utf8(output.stdout)?
            .lines()
            .filter_map(|s| s.strip_prefix('v'))
            .filter_map(|s| Version::parse(s).ok())
            .filter(|v| v.pre.is_empty())
            .filter(|v| line_claims_version(line, v))
            .max();
        Ok(latest)
    }

    /// Resolve `--from`: explicit override, or line-specific default.
    fn resolve_from(&self, line: &Line, kind: &NewVersion) -> Result<String> {
        if let Some(from) = &self.from {
            return Ok(from.clone());
        }
        // Line-specific defaults.
        let default = match (line, kind) {
            // Tip patch: from the latest released tag.  Dev moves too fast
            // for patches — use the released commit as the base.
            (Line::Tip, NewVersion::Patch) => {
                let latest = self.latest_release_on_line(line)?.ok_or_else(|| {
                    anyhow!(
                        "no prior release tag on tip; can't default --from for a patch.  \
                         Pass --from <commitish> explicitly."
                    )
                })?;
                format!("v{latest}")
            }
            // Tip minor or major: from dev.
            (Line::Tip, _) => line.dev_branch(),
            // LTS or staging: always from the line's dev branch.
            // For LTS that's already the locked-down trunk; for staging it's
            // the only thing that exists yet.
            (_, _) => line.dev_branch(),
        };
        Ok(default)
    }

    fn check_no_existing_release_pr(&self, version: &Version) -> Result<()> {
        if !xtask::gh::available() {
            return Err(anyhow!(
                "the `gh` CLI is required for release PR detection (install from https://cli.github.com/)"
            ));
        }
        let head = version.to_string();
        let prs: Vec<state::PrSummary> = xtask::gh::json([
            "--repo",
            self.common.repo.as_str(),
            "pr",
            "list",
            "--state",
            "open",
            "--head",
            head.as_str(),
            "--json",
            "number,title,headRefName,isDraft,mergeStateStatus,url",
        ])?;
        if let Some(pr) = prs.first() {
            return Err(anyhow!(
                "release PR for v{version} already exists: #{} {}; close or finish it before cutting a new one",
                pr.number,
                pr.url.as_deref().unwrap_or(""),
            ));
        }
        Ok(())
    }

    fn maybe_add_empty_commit(
        &self,
        wt: &xtask::worktree::WorkTree,
        line: &Line,
        version: &Version,
    ) -> Result<()> {
        let main = line.main_branch();
        let range = format!(
            "{origin}/{main}..HEAD",
            origin = self.common.origin,
            main = main
        );
        let count_str = wt.git_out(["rev-list", "--count", range.as_str()])?;
        let count: usize = count_str.trim().parse()?;
        if count == 0 {
            eprintln!(
                "{}",
                style(format!(
                    "no commits between {origin}/{main} and HEAD — adding empty commit so the PR can be opened",
                    origin = self.common.origin,
                ))
                .dim()
            );
            wt.git([
                "commit",
                "--allow-empty",
                "-m",
                &format!("Start v{version} PR"),
            ])?;
        } else {
            eprintln!(
                "{}",
                style(format!(
                    "branch has {count} commit(s) ahead of {origin}/{main} — no empty commit needed",
                    origin = self.common.origin,
                ))
                .dim()
            );
        }
        Ok(())
    }

    fn open_release_pr(&self, line: &Line, version: &Version) -> Result<()> {
        let main = line.main_branch();
        let branch = version.to_string();
        let title = format!("release: v{version}");
        let body = release_pr_body(line, version);

        if !xtask::gh::available() {
            return Err(anyhow!("the `gh` CLI is required to open the release PR"));
        }
        xtask::gh::run([
            "--repo",
            self.common.repo.as_str(),
            "pr",
            "create",
            "--draft",
            "--label",
            "release",
            "-B",
            main.as_str(),
            "-H",
            branch.as_str(),
            "--title",
            title.as_str(),
            "--body",
            body.as_str(),
        ])?;
        Ok(())
    }

    /// Pretty-print the resolved plan for the user.
    fn print_plan(&self, line: &Line, kind: &NewVersion, target: &Version, from: &str) {
        let kind_label = match kind {
            NewVersion::Patch => "patch",
            NewVersion::Minor => "minor",
            NewVersion::Major => "major",
            NewVersion::Specific(_) => "specific",
        };
        eprintln!();
        eprintln!("{}", style("Cutting a new release.").bold());
        eprintln!();
        eprintln!(
            "  {:<12} {} ({})",
            style("Line:").dim(),
            style(line.id()).cyan(),
            style(format!("{} / {}", line.main_branch(), line.dev_branch())).dim(),
        );
        eprintln!(
            "  {:<12} {} ({} bump)",
            style("Version:").dim(),
            style(format!("v{target}")).cyan().bold(),
            style(kind_label).dim(),
        );
        eprintln!(
            "  {:<12} {}",
            style("Branch:").dim(),
            style(target.to_string()).cyan(),
        );
        eprintln!("  {:<12} {}", style("Cut from:").dim(), style(from).cyan(),);
        eprintln!(
            "  {:<12} {}",
            style("PR target:").dim(),
            style(line.main_branch()).cyan(),
        );
        eprintln!();
    }

    /// Print the dry-run command sequence that would execute.
    fn print_dry_run_steps(&self, line: &Line, target: &Version, from: &str) {
        let branch = target.to_string();
        let main = line.main_branch();
        let origin = &self.common.origin;
        let repo = &self.common.repo;
        let wt_path = self.worktree_path.clone().unwrap_or_else(|| {
            xtask::worktree::default_path(
                PKG_PROJECT_ROOT.as_std_path(),
                &format!("release-new-{target}"),
            )
        });
        eprintln!("{}", style("(dry-run) — would run:").yellow());
        eprintln!();
        eprintln!("  {} ", style("# fetch in main repo").dim());
        eprintln!("  git fetch --tags --prune {origin}");
        eprintln!();
        eprintln!(
            "  {} {}",
            style("# isolated worktree:").dim(),
            style(wt_path.display()).cyan(),
        );
        eprintln!("  git worktree add {} {from}", wt_path.display());
        eprintln!();
        eprintln!("  {}", style("# in worktree:").dim());
        eprintln!("  git checkout -b {branch}");
        eprintln!(
            "  git rev-list {origin}/{main}..HEAD --count   {}",
            style("# if 0: commit --allow-empty").dim()
        );
        eprintln!("  git push --set-upstream {origin} {branch}");
        eprintln!();
        eprintln!("  {}", style("# open PR (cwd-independent):").dim());
        eprintln!("  gh pr create --repo {repo} --draft --label release -B {main} -H {branch} \\");
        eprintln!("    --title \"release: v{target}\" --body <boilerplate>");
        eprintln!();
        eprintln!(
            "  {}",
            style("# cleanup (always on success; on failure: keep for debug):").dim()
        );
        eprintln!("  git worktree remove --force {}", wt_path.display());
        eprintln!();
    }
}

/// Free function so it can be unit-tested with a mocked tag lookup.
fn resolve_target_version<F>(line: &Line, kind: &NewVersion, latest_lookup: F) -> Result<Version>
where
    F: FnOnce(&Line) -> Result<Option<Version>>,
{
    match kind {
        NewVersion::Specific(v) => Ok(v.clone()),
        kind => {
            let latest = latest_lookup(line)?.ok_or_else(|| {
                anyhow!(
                    "no prior release tag found on line `{line}` to bump from; \
                     pass an explicit version like `2.14.1` instead of `{:?}`",
                    kind
                )
            })?;
            bump(&latest, kind)
        }
    }
}

/// Bump a version by kind.  Major bumps reset minor + patch; minor bumps
/// reset patch.  Patch increments patch.
fn bump(latest: &Version, kind: &NewVersion) -> Result<Version> {
    let mut v = latest.clone();
    match kind {
        NewVersion::Major => {
            v.major += 1;
            v.minor = 0;
            v.patch = 0;
        }
        NewVersion::Minor => {
            v.minor += 1;
            v.patch = 0;
        }
        NewVersion::Patch => {
            v.patch += 1;
        }
        NewVersion::Specific(_) => {
            return Err(anyhow!("internal: bump() called with NewVersion::Specific"));
        }
    }
    v.pre = semver::Prerelease::EMPTY;
    v.build = semver::BuildMetadata::EMPTY;
    Ok(v)
}

/// Whether `version` belongs to `line`.  Tip claims everything that no
/// LTS or staging line claims; LTS claims exact major+minor; staging
/// claims any major match (typically only pre-releases pre-first-minor).
fn line_claims_version(line: &Line, v: &Version) -> bool {
    match line {
        Line::Tip => true, // simplification: tip claims any tag for the bump-base lookup
        Line::Lts { major, minor } => v.major == *major && v.minor == *minor,
        Line::Staging { major } => v.major == *major,
    }
}

fn release_pr_body(line: &Line, version: &Version) -> String {
    let main = line.main_branch();
    format!(
"> **Note**
> **This particular PR must be true-merged to `{main}`.**

* This PR is only ready to review when it is marked as \"Ready for Review\".  It represents the merge to the `{main}` branch of an upcoming v{version} release.
* It will act as a staging branch until we are ready to finalize the release.
* We may cut any number of alpha and release candidate (RC) versions off this branch prior to formalizing it.
* Backports land here via Mergify (use the `backport-{version}` label on the source PR).
* This PR is **primarily a merge commit**, so reviewing every individual commit shown below is **not necessary** since those have been reviewed in their own PR.  However, things important to review on this PR **once it's marked \"Ready for Review\"**:
    - Does this PR target the right branch? (should be `{main}`)
    - Are the appropriate **version bumps** and **release note edits** in the end of the commit list (or within the last few commits).  In other words, \"Did the 'release prep' PR actually land on this branch?\"
    - If those things look good, this PR is good to merge!
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn bump_patch() {
        assert_eq!(bump(&v("2.14.0"), &NewVersion::Patch).unwrap(), v("2.14.1"));
        assert_eq!(bump(&v("2.10.2"), &NewVersion::Patch).unwrap(), v("2.10.3"));
    }

    #[test]
    fn bump_minor() {
        assert_eq!(bump(&v("2.14.0"), &NewVersion::Minor).unwrap(), v("2.15.0"));
        // Minor resets patch to 0.
        assert_eq!(bump(&v("2.14.5"), &NewVersion::Minor).unwrap(), v("2.15.0"));
    }

    #[test]
    fn bump_major() {
        assert_eq!(bump(&v("2.14.0"), &NewVersion::Major).unwrap(), v("3.0.0"));
        // Major resets minor + patch to 0.
        assert_eq!(bump(&v("2.14.5"), &NewVersion::Major).unwrap(), v("3.0.0"));
    }

    #[test]
    fn bump_strips_prerelease_metadata() {
        // If the latest tag was a pre-release somehow, bumping should land on
        // a clean release version.
        let pre = v("2.14.0-rc.2");
        assert_eq!(bump(&pre, &NewVersion::Patch).unwrap(), v("2.14.1"));
    }

    #[test]
    fn newversion_parses_kinds() {
        assert!(matches!(
            NewVersion::from_str("patch").unwrap(),
            NewVersion::Patch
        ));
        assert!(matches!(
            NewVersion::from_str("minor").unwrap(),
            NewVersion::Minor
        ));
        assert!(matches!(
            NewVersion::from_str("major").unwrap(),
            NewVersion::Major
        ));
    }

    #[test]
    fn newversion_parses_specific() {
        match NewVersion::from_str("2.14.1").unwrap() {
            NewVersion::Specific(ver) => assert_eq!(ver, v("2.14.1")),
            _ => panic!("expected Specific"),
        }
    }

    #[test]
    fn newversion_rejects_garbage() {
        assert!(NewVersion::from_str("").is_err());
        assert!(NewVersion::from_str("not-a-version").is_err());
        assert!(NewVersion::from_str("2.14").is_err());
    }

    #[test]
    fn line_claims_lts() {
        let lts = Line::Lts {
            major: 2,
            minor: 10,
        };
        assert!(line_claims_version(&lts, &v("2.10.0")));
        assert!(line_claims_version(&lts, &v("2.10.5")));
        assert!(!line_claims_version(&lts, &v("2.11.0")));
        assert!(!line_claims_version(&lts, &v("3.0.0")));
    }

    #[test]
    fn line_claims_staging() {
        let staging = Line::Staging { major: 3 };
        assert!(line_claims_version(&staging, &v("3.0.0")));
        assert!(line_claims_version(&staging, &v("3.5.0")));
        assert!(!line_claims_version(&staging, &v("2.14.0")));
    }
}
