use anyhow::Result;
use anyhow::anyhow;
use xtask::*;

pub struct PreVerify();

impl PreVerify {
    pub fn run() -> Result<()> {
        let version = format!("v{}", *PKG_VERSION);

        // Get the git tag name as a string
        let tags_output = std::process::Command::new("git")
            .args(["describe", "--tags", "--exact-match"])
            .output()
            .map_err(|e| {
                anyhow!(
                    "failed to execute 'git describe --tags --exact-match': {}",
                    e
                )
            })?
            .stdout;
        let tags_raw = String::from_utf8_lossy(&tags_output);
        let tags_list = tags_raw
            .split("\n")
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>();

        // If the tags contains the version, then we're good
        if tags_list.is_empty() {
            return Err(anyhow!(
                "release cannot be performed because current git tree is not tagged"
            ));
        }
        if !tags_list.contains(&version.as_str()) {
            return Err(anyhow!(
                "the git tree tags {{{}}} does not contain the version {} from the Cargo.toml",
                tags_list.join(", "),
                version
            ));
        }
        Ok(())
    }
}
