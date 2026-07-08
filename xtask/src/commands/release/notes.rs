//! Release-notes extraction and (optionally) publishing.
//!
//! `cargo xtask release notes <version> [--publish]`:
//!
//! - Extracts the `# [<version>]` section from `CHANGELOG.md` (pulldown-cmark
//!   walks the doc — resilient to `#` characters inside code blocks).
//! - Applies the `[@user](https://github.com/user)` → `@user` transform where
//!   display name matches the URL slug.  Preserves genuine display/account
//!   mismatches.
//! - Default is dry-run — prints the transformed body to stdout.
//! - `--publish` calls `gh release edit <tag> --notes-file <extracted>` to
//!   populate the GitHub Release body.  Refuses to overwrite an already-non-
//!   empty body unless `--force` is set.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use pulldown_cmark::Event;
use pulldown_cmark::HeadingLevel;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use regex::Regex;

/// Extract a version's CHANGELOG section, optionally posting to the release.
#[derive(Debug, clap::Parser)]
pub struct Notes {
    /// Version to extract (e.g., "2.16.0").
    pub version: String,

    /// Tag name (defaults to `v<version>`).
    #[arg(long)]
    pub tag: Option<String>,

    /// Path to CHANGELOG.md.
    #[arg(long, default_value = "CHANGELOG.md")]
    pub file: PathBuf,

    /// Actually POST to the GitHub release.  Default is dry-run.
    #[arg(long)]
    pub publish: bool,

    /// Overwrite the release body even if it is already non-empty.
    #[arg(long)]
    pub force: bool,
}

impl Notes {
    pub fn run(&self) -> Result<()> {
        let tag = self
            .tag
            .clone()
            .unwrap_or_else(|| format!("v{}", self.version));

        let slice = extract(&self.version, &self.file)?;
        let body = transform_github_user_links(&slice);

        if !self.publish {
            eprintln!("(dry-run — not writing to release {})", tag);
            eprintln!("--- release body preview ({} bytes) ---", body.len());
            println!("{}", body);
            return Ok(());
        }

        if !self.force {
            let existing = read_release_body(&tag)?;
            if !existing.trim().is_empty() {
                bail!(
                    "release {} already has a non-empty body ({} bytes); pass --force to overwrite",
                    tag,
                    existing.len()
                );
            }
        }

        write_release_body(&tag, &body)?;
        eprintln!("Wrote {} bytes to release {}.", body.len(), tag);
        Ok(())
    }
}

fn extract(version: &str, file: &PathBuf) -> Result<String> {
    let content =
        fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;

    let headings = collect_h1_headings(&content);
    let target = format!("[{}]", version);

    let idx = headings
        .iter()
        .position(|h| h.text.contains(&target))
        .ok_or_else(|| anyhow!("no `# [{}]` heading found in {}", version, file.display()))?;

    let start = headings[idx].end_offset;
    let end = headings
        .get(idx + 1)
        .map(|h| h.start_offset)
        .unwrap_or(content.len());

    Ok(content[start..end].trim().to_string())
}

struct H1Heading {
    start_offset: usize,
    end_offset: usize,
    text: String,
}

fn collect_h1_headings(content: &str) -> Vec<H1Heading> {
    let mut headings = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut current_text = String::new();

    for (event, range) in Parser::new(content).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => {
                current_start = Some(range.start);
                current_text.clear();
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => {
                if let Some(start) = current_start.take() {
                    headings.push(H1Heading {
                        start_offset: start,
                        end_offset: range.end,
                        text: std::mem::take(&mut current_text),
                    });
                }
            }
            Event::Text(t) | Event::Code(t) if current_start.is_some() => {
                current_text.push_str(&t);
            }
            _ => {}
        }
    }

    headings
}

/// Transform `[@user](https://github.com/user)` → `@user`.
///
/// Only replaces links where the display name (after `@`) matches the URL
/// slug — matches Apollo's changeset convention.  Leaves PR/Issue links
/// (`[Issue/PR #123](url)`) alone.
fn transform_github_user_links(input: &str) -> String {
    let re = Regex::new(r"\[@([\w-]+)\]\(https://github\.com/([\w-]+)\)").unwrap();
    re.replace_all(input, |caps: &regex::Captures| {
        let display = &caps[1];
        let slug = &caps[2];
        if display == slug {
            format!("@{}", display)
        } else {
            caps[0].to_string()
        }
    })
    .into_owned()
}

fn read_release_body(tag: &str) -> Result<String> {
    let output = Command::new("gh")
        .args(["release", "view", tag, "--json", "body", "--jq", ".body"])
        .output()
        .context("running `gh release view`")?;
    if !output.status.success() {
        bail!(
            "gh release view {} failed: {}",
            tag,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn write_release_body(tag: &str, body: &str) -> Result<()> {
    let tmp = tempfile::NamedTempFile::new().context("creating temp file")?;
    fs::write(tmp.path(), body).context("writing notes to temp file")?;

    let output = Command::new("gh")
        .args([
            "release",
            "edit",
            tag,
            "--notes-file",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .context("running `gh release edit`")?;
    if !output.status.success() {
        bail!(
            "gh release edit {} failed: {}",
            tag,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Changelog

Intro paragraph.

# [2.16.0] - 2026-06-30

## 🚀 Features

### Feature A ([PR #1](https://example/1))

Body of A.

By [@alice](https://github.com/alice) in https://github.com/apollographql/router/pull/1

# [2.15.1] - 2026-06-10

## 🐛 Fixes

### Fix B ([PR #2](https://example/2))

Body of B.

# [2.15.0] - 2026-05-26

Body of 2.15.0.
";

    fn extract_from(content: &str, version: &str) -> Result<String> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), content).unwrap();
        extract(version, &tmp.path().to_path_buf())
    }

    #[test]
    fn extracts_middle_section() {
        let out = extract_from(SAMPLE, "2.15.1").unwrap();
        assert!(out.starts_with("## 🐛 Fixes"));
        assert!(out.contains("Fix B"));
        assert!(!out.contains("Feature A"));
        assert!(!out.contains("[2.15.0]"));
    }

    #[test]
    fn extracts_top_section() {
        let out = extract_from(SAMPLE, "2.16.0").unwrap();
        assert!(out.starts_with("## 🚀 Features"));
        assert!(out.contains("Feature A"));
        assert!(!out.contains("Fix B"));
    }

    #[test]
    fn extracts_last_section() {
        let out = extract_from(SAMPLE, "2.15.0").unwrap();
        assert_eq!(out.trim(), "Body of 2.15.0.");
    }

    #[test]
    fn missing_version_errors() {
        let err = extract_from(SAMPLE, "9.9.9").unwrap_err();
        assert!(err.to_string().contains("9.9.9"));
    }

    #[test]
    fn code_block_hash_is_not_a_heading() {
        let content = "\
# [1.0.0] - 2026-01-01

Some content.

```
# not a heading
```

More content.

# [0.9.0] - 2025-12-01

Old.
";
        let out = extract_from(content, "1.0.0").unwrap();
        assert!(out.contains("# not a heading"));
        assert!(!out.contains("Old."));
    }

    #[test]
    fn transform_replaces_matching_user_link() {
        let out = transform_github_user_links("By [@alice](https://github.com/alice) in ...");
        assert_eq!(out, "By @alice in ...");
    }

    #[test]
    fn transform_leaves_mismatched_link_alone() {
        let out = transform_github_user_links("[@alice](https://github.com/bob)");
        assert_eq!(out, "[@alice](https://github.com/bob)");
    }

    #[test]
    fn transform_leaves_pr_links_alone() {
        let input = "See [PR #123](https://github.com/apollographql/router/pull/123).";
        let out = transform_github_user_links(input);
        assert_eq!(out, input);
    }

    #[test]
    fn transform_handles_multiple_users_on_one_line() {
        let input =
            "By [@alice](https://github.com/alice) and [@bob-42](https://github.com/bob-42)";
        let out = transform_github_user_links(input);
        assert_eq!(out, "By @alice and @bob-42");
    }
}
