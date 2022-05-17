mod plugin;

use crate::plugin::PluginAction;
use anyhow::Result;
use clap::Subcommand;

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
    use anyhow::Result;
    use cargo_scaffold::{Opts, ScaffoldDescription};
    use inflector::Inflector;
    use std::collections::BTreeMap;
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn test_scaffold() -> Result<()> {
        let temp_dir = tempfile::Builder::new()
            .prefix("router_scaffold")
            .tempdir()?;
        let current_dir = env::current_dir()?;
        // Scaffold the main project
        let opts = Opts::builder()
            .project_name("temp")
            .target_dir(temp_dir.path())
            .template_path("templates/base")
            .force(true)
            .build();
        ScaffoldDescription::new(opts)?.scaffold_with_parameters(BTreeMap::from([(
            "integration_test".to_string(),
            toml::Value::String(
                current_dir
                    .to_str()
                    .expect("current dir must be convertable to string")
                    .to_string(),
            ),
        )]))?;
        test_build(&temp_dir)?;

        // Scaffold one of each type of plugin
        scaffold_plugin(&current_dir, &temp_dir, "basic")?;
        scaffold_plugin(&current_dir, &temp_dir, "auth")?;
        scaffold_plugin(&current_dir, &temp_dir, "tracing")?;
        std::fs::write(
            temp_dir.path().join("src/plugins/mod.rs"),
            "mod auth;\nmod basic;\nmod tracing;\n",
        )?;
        test_build(&temp_dir)?;

        drop(temp_dir);
        Ok(())
    }

    fn scaffold_plugin(current_dir: &PathBuf, dir: &TempDir, plugin_type: &str) -> Result<()> {
        let opts = Opts::builder()
            .project_name(plugin_type)
            .target_dir(dir.path())
            .append(true)
            .template_path("templates/plugin")
            .build();
        ScaffoldDescription::new(opts)?.scaffold_with_parameters(BTreeMap::from([
            (
                format!("type_{}", plugin_type),
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
                    current_dir
                        .to_str()
                        .expect("current dir must be convertable to string")
                        .to_string(),
                ),
            ),
        ]))?;
        Ok(())
    }

    fn test_build(dir: &TempDir) -> Result<()> {
        let output = Command::new("cargo")
            .args(["test"])
            .current_dir(dir)
            .output()?;
        if !output.status.success() {
            eprintln!("failed to build scaffolded project");
            eprintln!("{}", String::from_utf8(output.stdout)?);
            eprintln!("{}", String::from_utf8(output.stderr)?);
            panic!(
                "build failed with exit code {}",
                output.status.code().unwrap_or_default()
            );
        }
        Ok(())
    }
}
