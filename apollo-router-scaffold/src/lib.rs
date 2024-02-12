mod plugin;

use anyhow::Result;
use clap::Subcommand;

use crate::plugin::PluginAction;

#[derive(Subcommand, Debug)]
pub enum RouterAction {
    /// Manage plugins
    Plugin {
        #[clap(subcommand)]
        action: PluginAction,
    },
}

impl RouterAction {
    pub fn execute(&self) -> Result<()> {
        match self {
            RouterAction::Plugin { action } => action.execute(),
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;
    use std::env;
    use std::path::Path;
    use std::path::PathBuf;
    use std::path::MAIN_SEPARATOR;
    use std::process::Command;

    use anyhow::bail;
    use anyhow::Result;
    use cargo_scaffold::Opts;
    use cargo_scaffold::ScaffoldDescription;
    use inflector::Inflector;
    use tempfile::TempDir;

    #[test]
    fn the_next_test_takes_a_while_to_pass_do_not_worry() {}

    #[test]
    // this test takes a while, I hope the above test name
    // let users know they should not worry and wait a bit.
    // Hang in there!
    fn test_scaffold() {
        let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let repo_root = manifest_dir.parent().unwrap();
        let target_dir = repo_root.join("target");
        assert!(target_dir.exists());
        let temp_dir = tempfile::Builder::new()
            .prefix("router_scaffold")
            .tempdir()
            .unwrap();
        std::fs::copy(
            repo_root.join("rust-toolchain.toml"),
            temp_dir.path().join("rust-toolchain.toml"),
        )
        .unwrap();

        let current_dir = env::current_dir().unwrap();
        // Scaffold the main project
        let opts = Opts::builder(PathBuf::from("templates").join("base"))
            .project_name("temp")
            .target_dir(temp_dir.path())
            .force(true);
        ScaffoldDescription::new(opts)
            .unwrap()
            .scaffold_with_parameters(BTreeMap::from([(
                "integration_test".to_string(),
                toml::Value::String(
                    format!(
                        "{}{}",
                        current_dir
                            .parent()
                            .expect("current dir cannot be the root")
                            .to_str()
                            .expect("current dir must be convertable to string"),
                        // add / or \ depending on windows or unix
                        MAIN_SEPARATOR,
                    )
                    // we need to double \ so they don't get interpreted as escape characters in TOML
                    .replace('\\', "\\\\"),
                ),
            )]))
            .unwrap();
        std::fs::copy(
            repo_root.join("Cargo.lock"),
            temp_dir.path().join("Cargo.lock"),
        )
        .unwrap();
        let main = temp_dir.path().join("src").join("main.rs");
        std::fs::write(
            &main,
            format!(
                "#![deny(warnings)]\n{}",
                std::fs::read_to_string(&main).unwrap()
            ),
        )
        .unwrap();
        let _ = test_build_with_backup_folder(&temp_dir, &target_dir);

        // Scaffold one of each type of plugin
        scaffold_plugin(&current_dir, &temp_dir, "basic").unwrap();
        scaffold_plugin(&current_dir, &temp_dir, "auth").unwrap();
        scaffold_plugin(&current_dir, &temp_dir, "tracing").unwrap();
        std::fs::write(
            temp_dir.path().join("src").join("plugins").join("mod.rs"),
            "mod auth;\nmod basic;\nmod tracing;\n",
        )
        .unwrap();

        test_build_with_backup_folder(&temp_dir, &target_dir).unwrap()
    }

    fn scaffold_plugin(current_dir: &Path, dir: &TempDir, plugin_type: &str) -> Result<()> {
        let opts = Opts::builder(PathBuf::from("templates").join("plugin"))
            .project_name(plugin_type)
            .target_dir(dir.path())
            .append(true);
        ScaffoldDescription::new(opts)?.scaffold_with_parameters(BTreeMap::from([
            (
                format!("type_{plugin_type}"),
                toml::Value::String(plugin_type.to_string()),
            ),
            (
                "snake_name".to_string(),
                toml::Value::String(plugin_type.to_snake_case()),
            ),
            (
                "pascal_name".to_string(),
                toml::Value::String(plugin_type.to_pascal_case()),
            ),
            (
                "project_name".to_string(),
                toml::Value::String("acme".to_string()),
            ),
            (
                "integration_test".to_string(),
                toml::Value::String(
                    format!(
                        "{}{}",
                        current_dir
                            .parent()
                            .expect("current dir cannot be the root")
                            .to_str()
                            .expect("current dir must be convertable to string"),
                        // add / or \ depending on windows or unix
                        MAIN_SEPARATOR,
                    )
                    // we need to double \ so they don't get interpreted as escape characters in TOML
                    .replace('\\', "\\\\"),
                ),
            ),
        ]))?;
        Ok(())
    }

    fn test_build_with_backup_folder(temp_dir: &TempDir, target_dir: &Path) -> Result<()> {
        test_build(temp_dir, target_dir).map_err(|e| {
            let mut output_dir = std::env::temp_dir();
            output_dir.push("test_scaffold_output");

            // best effort to prepare the output directory
            let _ = std::fs::remove_dir_all(&output_dir);
            copy_dir::copy_dir(temp_dir, &output_dir)
                .expect("couldn't copy test_scaffold_output directory");
            anyhow::anyhow!(
                "scaffold test failed: {e}\nYou can find the scaffolded project at '{}'",
                output_dir.display()
            )
        })
    }

    fn test_build(dir: &TempDir, target_dir: &Path) -> Result<()> {
        let output = Command::new("cargo")
            .args(["test"])
            .env("CARGO_TARGET_DIR", target_dir)
            .current_dir(dir)
            .output()?;
        if !output.status.success() {
            eprintln!("failed to build scaffolded project");
            eprintln!("{}", String::from_utf8(output.stdout)?);
            eprintln!("{}", String::from_utf8(output.stderr)?);
            bail!(
                "build failed with exit code {}",
                output.status.code().unwrap_or_default()
            );
        }
        Ok(())
    }
}
