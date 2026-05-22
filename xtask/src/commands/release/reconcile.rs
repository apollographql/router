use anyhow::Result;
use anyhow::anyhow;
use dialoguer::Confirm;
use dialoguer::Input;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use semver::Version;
use xtask::*;

use super::common::Line;
use super::common::ReleaseCommonArgs;
use super::state;

/// Open (or finish) the reconcile-main-back-to-dev PR for a released version.
/// Works on any release line — tip (`main` → `dev`) or an LTS line (e.g.,
/// `main-v2.10.x` → `dev-v2.10.x`).
#[derive(Debug, clap::Parser)]
pub struct Reconcile {
    #[command(flatten)]
    pub common: ReleaseCommonArgs,

    /// The version being reconciled (e.g., `2.14.1`).  Used for PR title and
    /// local branch name (`reconcile-v<VERSION>`).  If omitted and running
    /// interactively, you'll be prompted with the line's latest released tag
    /// as a default.
    #[clap(long)]
    pub version: Option<Version>,

    /// Skip setting auto-merge on the reconciliation PR.
    #[clap(long)]
    pub no_auto_merge: bool,

    /// Print what would run without executing anything.
    #[clap(long)]
    pub dry_run: bool,

    /// Resume a reconcile that was interrupted for conflict resolution.
    /// Skips the initial pristine-check + branch creation + merge, assumes
    /// the `reconcile-v<VERSION>` branch is already at the commit you want
    /// to push, and proceeds from the push onward.
    #[clap(long)]
    pub resume: bool,
}

impl Reconcile {
    pub fn run(&self) -> Result<()> {
        let line = self.resolve_line()?;
        let version = self.resolve_version(&line)?;

        eprintln!(
            "Reconciling v{version} on line `{line}`:  {main} → {dev}",
            main = line.main_branch(),
            dev = line.dev_branch()
        );

        if self.dry_run {
            eprintln!("(dry-run) — no commands will execute");
        }

        if self.resume {
            return self.do_push_and_pr(&line, &version);
        }

        self.check_no_existing_reconcile(&line, &version)?;
        self.ensure_pristine_checkout()?;
        self.do_merge(&line, &version)?;
        self.do_push_and_pr(&line, &version)?;

        Ok(())
    }

    fn resolve_line(&self) -> Result<Line> {
        if let Some(line) = self.common.resolve_line() {
            return Ok(line);
        }

        if self.common.non_interactive {
            return Err(anyhow!(
                "could not infer release line from current branch; pass --line tip or --line <major>.<minor>.x"
            ));
        }

        // Interactive: list detected lines via state detection and let user pick.
        eprintln!("No --line flag and current branch doesn't match a known main/dev pair.");
        eprintln!("Fetching release lines from `{}`...", self.common.origin);
        state::fetch(&self.common.origin)?;
        let detected = state::detect_state(&self.common.repo, &self.common.origin)?;
        if detected.lines.is_empty() {
            return Err(anyhow!("no release lines detected on remote"));
        }

        let items: Vec<String> = detected
            .lines
            .iter()
            .map(|ls| {
                let suffix = if ls.line.line.is_lts() { " (LTS)" } else { "" };
                format!(
                    "{}{}  —  {} / {}",
                    ls.line.line.id(),
                    suffix,
                    ls.line.line.main_branch(),
                    ls.line.line.dev_branch()
                )
            })
            .collect();

        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Which line are you reconciling?")
            .items(&items)
            .default(0)
            .interact()?;

        Ok(detected.lines[selection].line.line.clone())
    }

    fn resolve_version(&self, line: &Line) -> Result<Version> {
        if let Some(v) = &self.version {
            return Ok(v.clone());
        }

        if self.common.non_interactive {
            return Err(anyhow!("--version is required in non-interactive mode"));
        }

        // Fetch the latest released tag on this line as a suggested default.
        state::fetch(&self.common.origin)?;
        let detected = state::detect_state(&self.common.repo, &self.common.origin)?;
        let default = detected
            .lines
            .iter()
            .find(|ls| ls.line.line == *line)
            .and_then(|ls| ls.latest_release.clone())
            .map(|v| v.to_string())
            .unwrap_or_default();

        let input: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("Version being reconciled on line `{line}`"))
            .with_initial_text(default)
            .interact_text()?;

        Version::parse(input.trim()).map_err(|e| anyhow!("invalid version `{input}`: {e}"))
    }

    fn check_no_existing_reconcile(&self, _line: &Line, version: &Version) -> Result<()> {
        let detected = state::detect_state(&self.common.repo, &self.common.origin)?;
        let existing = detected
            .lines
            .iter()
            .flat_map(|ls| &ls.in_progress)
            .find(|vw| vw.version == *version && vw.reconcile_pr.is_some())
            .and_then(|vw| vw.reconcile_pr.as_ref());

        if let Some(pr) = existing {
            eprintln!(
                "A reconcile PR for v{version} is already open: #{} {}",
                pr.number,
                pr.url.as_deref().unwrap_or("")
            );
            return Err(anyhow!(
                "reconcile PR #{} already exists for v{version}; close or merge it before re-running",
                pr.number
            ));
        }
        Ok(())
    }

    fn ensure_pristine_checkout(&self) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        let output = std::process::Command::new(which::which("git")?)
            .args(["status", "--untracked-files=no", "--porcelain"])
            .output()?;
        if !output.stdout.is_empty() {
            return Err(anyhow!(
                "git workspace is not clean; commit or stash changes before reconciling"
            ));
        }
        Ok(())
    }

    fn do_merge(&self, line: &Line, version: &Version) -> Result<()> {
        let main = line.main_branch();
        let dev = line.dev_branch();
        let reconcile_branch = format!("reconcile-v{version}");

        let commit_message = format!("Reconcile `{dev}` after merge to `{main}` for v{version}");

        if self.dry_run {
            eprintln!("(dry-run) would run:");
            eprintln!(
                "  git fetch --tags --prune {origin}",
                origin = self.common.origin
            );
            eprintln!("  git checkout {main}");
            eprintln!("  git pull {origin} {main}", origin = self.common.origin);
            eprintln!("  git checkout -b {reconcile_branch}");
            eprintln!(
                "  git merge --no-ff -m \"{commit_message}\" {origin}/{dev}",
                origin = self.common.origin
            );
            return Ok(());
        }

        git!(["fetch", "--tags", "--prune", self.common.origin.as_str()]);
        git!(["checkout", main.as_str()]);
        git!(["pull", self.common.origin.as_str(), main.as_str()]);
        git!(["checkout", "-b", reconcile_branch.as_str()]);

        // Run `git merge` manually so we can detect conflicts vs. success.
        let status = std::process::Command::new(which::which("git")?)
            .args([
                "merge",
                "--no-ff",
                "-m",
                commit_message.as_str(),
                &format!("{origin}/{dev}", origin = self.common.origin),
            ])
            .status()?;

        if !status.success() {
            return Err(anyhow!(
                "merge of `{origin}/{dev}` had conflicts.  Resolve them locally, commit \
                 with `git commit --no-edit`, then re-run with `--resume` to open the PR:\n\
                 \n\
                 cargo xtask release reconcile --line {line} --version {version} --resume",
                origin = self.common.origin,
            ));
        }

        Ok(())
    }

    fn do_push_and_pr(&self, line: &Line, version: &Version) -> Result<()> {
        let dev = line.dev_branch();
        let reconcile_branch = format!("reconcile-v{version}");
        let pr_title = format!(
            "Reconcile `{dev}` after merge to `{main}` for v{version}",
            main = line.main_branch()
        );
        let pr_body = format!(
            "Follow-up to the v{version} release, bringing version bumps and changelog \
             updates from `{main}` into the `{dev}` branch.\n\n\
             **This PR must be true-merged (NOT squashed, NOT rebased) into `{dev}`.**",
            main = line.main_branch()
        );

        if self.dry_run {
            eprintln!("(dry-run) would run:");
            eprintln!(
                "  git push --set-upstream {origin} {reconcile_branch}",
                origin = self.common.origin
            );
            eprintln!(
                "  gh pr create --repo {repo} -B {dev} -H {reconcile_branch} --title ... --body ...",
                repo = self.common.repo
            );
            if !self.no_auto_merge {
                eprintln!(
                    "  gh pr merge --repo {repo} --merge --auto {reconcile_branch}",
                    repo = self.common.repo
                );
            }
            return Ok(());
        }

        if !xtask::gh::available() {
            return Err(anyhow!(
                "the `gh` CLI is required to open the reconcile PR (install from https://cli.github.com/ and run `gh auth login`)"
            ));
        }

        // Interactive confirmation before pushing (unless non-interactive).
        if !self.common.non_interactive {
            let proceed = Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(format!(
                    "Push `{reconcile_branch}` to {origin} and open PR into `{dev}`?",
                    origin = self.common.origin
                ))
                .default(true)
                .interact()?;
            if !proceed {
                eprintln!("Aborted by user.");
                return Ok(());
            }
        }

        git!([
            "push",
            "--set-upstream",
            self.common.origin.as_str(),
            reconcile_branch.as_str(),
        ]);

        xtask::gh::run([
            "--repo",
            self.common.repo.as_str(),
            "pr",
            "create",
            "-B",
            dev.as_str(),
            "-H",
            reconcile_branch.as_str(),
            "--title",
            pr_title.as_str(),
            "--body",
            pr_body.as_str(),
        ])?;

        if !self.no_auto_merge {
            xtask::gh::run([
                "--repo",
                self.common.repo.as_str(),
                "pr",
                "merge",
                "--merge",
                "--auto",
                reconcile_branch.as_str(),
            ])?;
        }

        eprintln!("Reconciliation PR opened for v{version}.");

        Ok(())
    }
}
