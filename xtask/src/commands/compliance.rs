use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use serde_json::Value;
use xtask::*;

#[derive(Debug, clap::Parser, Default)]
pub struct Compliance {
    /// Fail only on security advisories that the merge-base of HEAD and this
    /// git revision does not already have. Licenses, bans and sources are
    /// checked as usual.
    #[clap(long, value_name = "REV")]
    advisory_base: Option<String>,
}

impl Compliance {
    pub fn run(&self) -> Result<()> {
        let Some(base_ref) = &self.advisory_base else {
            cargo!(["deny", "-L", "error", "check"]);
            return Ok(());
        };
        let policy = check_policy();
        let advisories = check_introduced_advisories(base_ref);
        // Only one error can be returned, so print the advisory one rather than lose it.
        if let (Err(_), Err(err)) = (&policy, &advisories) {
            eprintln!("Error: {err:?}");
        }
        policy.and(advisories)
    }
}

fn check_policy() -> Result<()> {
    cargo!([
        "deny", "-L", "error", "check", "bans", "licenses", "sources"
    ]);
    Ok(())
}

fn check_introduced_advisories(base_ref: &str) -> Result<()> {
    let base = merge_base(base_ref).with_context(|| {
        format!(
            "could not find where HEAD branched from `{base_ref}`; make sure it has been fetched"
        )
    })?;
    let base_tree = tempfile::tempdir()?;
    export_tree(&base, base_tree.path())
        .with_context(|| format!("could not export {base} to compare advisories with"))?;

    let head_advisories = advisories(PKG_PROJECT_ROOT.join("Cargo.toml").as_std_path(), false)?;
    let base_manifest = base_tree.path().join("Cargo.toml");
    let base_manifest = base_manifest
        .to_str()
        .context("temp dir is not valid UTF-8")?;
    cargo!(["fetch", "--locked", "--manifest-path", base_manifest]);
    // Offline, so the base is checked against the advisory DB the HEAD check just fetched.
    let base_advisories = advisories(Path::new(base_manifest), true)?;

    let (introduced, existing): (Vec<_>, Vec<_>) = head_advisories
        .into_iter()
        .partition(|(id, _)| !base_advisories.contains_key(id));
    if !existing.is_empty() {
        let ids: Vec<_> = existing.iter().map(|(id, _)| id.as_str()).collect();
        eprintln!(
            "note: {base_ref} already had {} at {base}, where this branch started, so this check ignores {}. `cargo deny check advisories` lists them.",
            ids.join(", "),
            if ids.len() == 1 { "it" } else { "them" },
        );
    }
    ensure!(
        introduced.is_empty(),
        "this change adds security advisories that {base_ref} did not have at {base}, where this \
         branch started:\n\n{}\n\
         If {base_ref} has since fixed or exempted them, rebase onto it. Otherwise update or \
         remove the affected crates; `cargo deny check advisories` shows which dependencies pull \
         them in. If an advisory does not affect the router, add its ID to `ignore` in deny.toml \
         with a comment saying why.",
        introduced
            .iter()
            .map(|(id, advisory)| advisory.describe(id))
            .collect::<String>(),
    );
    eprintln!("advisories ok: this change adds none that {base_ref} did not have at {base}");
    Ok(())
}

/// Returns the abbreviated hash of the merge-base of HEAD and `base_ref`.
fn merge_base(base_ref: &str) -> Result<String> {
    let base = git_out!(["merge-base", "HEAD", base_ref]);
    Ok(git_out!(["rev-parse", "--short", &base]))
}

/// Writes the files of commit `rev` to `dest`, without touching the checkout.
fn export_tree(rev: &str, dest: &Path) -> Result<()> {
    let mut git = Command::new(which::which("git")?)
        .args(["archive", rev])
        .current_dir(&*PKG_PROJECT_ROOT)
        .stdout(Stdio::piped())
        .spawn()?;
    tar::Archive::new(git.stdout.take().expect("stdout is piped")).unpack(dest)?;
    ensure!(git.wait()?.success(), "`git archive {rev}` failed");
    Ok(())
}

/// An advisory that cargo-deny reports as an error.
#[derive(Debug, PartialEq)]
struct Advisory {
    title: String,
    /// `name version` of each affected crate in the lockfile.
    crates: BTreeSet<String>,
}

impl Advisory {
    fn describe(&self, id: &str) -> String {
        let crates = self.crates.iter().cloned().collect::<Vec<_>>().join(", ");
        let mut text = format!("  {id} ({crates}): {}\n", self.title);
        if id.starts_with("RUSTSEC-") {
            text += &format!("    https://rustsec.org/advisories/{id}\n");
        }
        text
    }
}

/// Runs cargo-deny's advisory check on `manifest` with this checkout's
/// deny.toml, returning the errors it reports keyed by advisory ID.
fn advisories(manifest: &Path, offline: bool) -> Result<BTreeMap<String, Advisory>> {
    let mut command = Command::new(which::which("cargo")?);
    command
        .args(["deny", "--format", "json", "-L", "error", "--config"])
        .arg(PKG_PROJECT_ROOT.join("deny.toml"))
        .arg("--manifest-path")
        .arg(manifest);
    if offline {
        command.arg("--offline");
    }
    // Run from this checkout so the base tree resolves with the same toolchain and cargo config.
    let output = command
        .args(["check", "advisories"])
        .current_dir(&*PKG_PROJECT_ROOT)
        .output()?;
    // cargo-deny exits non-zero when it finds advisories, so whether it ran is
    // judged by the report it writes to stderr.
    let report = String::from_utf8_lossy(&output.stderr);
    parse_report(&report).with_context(|| {
        format!(
            "cargo-deny did not check advisories for {}:\n{report}",
            manifest.display()
        )
    })
}

fn parse_report(report: &str) -> Result<BTreeMap<String, Advisory>> {
    let mut advisories = BTreeMap::new();
    let mut summarised = false;
    for entry in report
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let fields = &entry["fields"];
        match entry["type"].as_str() {
            Some("summary") => summarised = true,
            Some("diagnostic") if fields["severity"] == "error" => {
                // Errors that aren't about an advisory are keyed by their diagnostic code.
                let id = fields["advisory"]["id"]
                    .as_str()
                    .or(fields["code"].as_str())
                    .context("diagnostic has neither an advisory ID nor a code")?;
                let advisory = advisories
                    .entry(id.to_string())
                    .or_insert_with(|| Advisory {
                        title: fields["message"].as_str().unwrap_or_default().to_string(),
                        crates: BTreeSet::new(),
                    });
                // A label's span reads `name version source`.
                let spans = fields["labels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|label| label["span"].as_str());
                for span in spans {
                    let name_version = span.split(' ').take(2).collect::<Vec<_>>().join(" ");
                    advisory.crates.insert(name_version);
                }
            }
            _ => {}
        }
    }
    ensure!(summarised, "the report has no summary");
    Ok(advisories)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_errors_from_cargo_deny_report() {
        let report = r#"{"fields":{"advisory":{"id":"RUSTSEC-2026-0382","package":"rustls-webpki"},"code":"vulnerability","labels":[{"column":1,"line":592,"message":"security vulnerability detected","span":"rustls-webpki 0.103.15 registry+https://github.com/rust-lang/crates.io-index"}],"message":"Certificate verification may accept distrusted intermediates","severity":"error"},"type":"diagnostic"}
{"fields":{"code":"yanked","labels":[{"span":"foo 1.0.0 registry+https://github.com/rust-lang/crates.io-index"}],"message":"detected yanked crate","severity":"warning"},"type":"diagnostic"}
{"fields":{"advisories":{"errors":1,"helps":0,"notes":0,"warnings":1}},"type":"summary"}"#;

        let advisories = parse_report(report).unwrap();

        assert_eq!(
            advisories,
            BTreeMap::from([(
                "RUSTSEC-2026-0382".to_string(),
                Advisory {
                    title: "Certificate verification may accept distrusted intermediates"
                        .to_string(),
                    crates: BTreeSet::from(["rustls-webpki 0.103.15".to_string()]),
                }
            )])
        );
    }

    #[test]
    fn rejects_report_without_summary() {
        assert!(parse_report("error: failed to fetch advisory database").is_err());
    }
}
