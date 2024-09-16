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

    use anyhow::Result;
    use cargo_scaffold::Opts;
    use cargo_scaffold::ScaffoldDescription;
    use dircmp::Comparison;
    use inflector::Inflector;
    use similar::ChangeTag;
    use similar::TextDiff;

    #[test]
    // this test takes a while, I hope the above test name
    // let users know they should not worry and wait a bit.
    // Hang in there!
    // Note that we configure nextest to use all threads for this test as invoking rustc will use all available CPU and cause timing tests to fail.
    fn test_scaffold() {
        let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let repo_root = manifest_dir.parent().unwrap();
        let target_dir = repo_root.join("target");
        assert!(target_dir.exists());
        let temp_dir = tempfile::Builder::new()
            .prefix("router_scaffold")
            .tempdir()
            .unwrap();
        let temp_dir_path = temp_dir.path();

        let current_dir = env::current_dir().unwrap();
        // Scaffold the main project
        let opts = Opts::builder(PathBuf::from("templates").join("base"))
            .project_name("temp")
            .target_dir(temp_dir_path)
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

        // Scaffold one of each type of plugin
        scaffold_plugin(&current_dir, temp_dir_path, "basic").unwrap();
        scaffold_plugin(&current_dir, temp_dir_path, "auth").unwrap();
        scaffold_plugin(&current_dir, temp_dir_path, "tracing").unwrap();
        std::fs::write(
            temp_dir.path().join("src").join("plugins").join("mod.rs"),
            "mod auth;\nmod basic;\nmod tracing;\n",
        )
        .unwrap();

        #[cfg(target_os = "windows")]
        let left = ".\\scaffold-test\\";
        #[cfg(not(target_os = "windows"))]
        let left = "./scaffold-test/";

        let cmp = Comparison::default();
        let diff = cmp
            .compare(left, temp_dir_path.to_str().unwrap())
            .expect("should compare");

        let mut found = false;
        if !diff.is_empty() {
            println!("generated scaffolding project has changed:\n{:#?}", diff);
            for file in diff.changed {
                println!("file: {file:?}");
                let file = PathBuf::from(file.to_str().unwrap().strip_prefix(left).unwrap());

                // we do not check the Cargo.toml files because they have differences due to import paths and workspace usage
                if file == PathBuf::from("Cargo.toml") || file == PathBuf::from("xtask/Cargo.toml")
                {
                    println!("skipping {}", file.to_str().unwrap());
                    continue;
                }
                // we are not dealing with windows line endings
                if file == PathBuf::from("src\\plugins\\mod.rs") {
                    println!("skipping {}", file.to_str().unwrap());
                    continue;
                }

                found = true;
                diff_file(&PathBuf::from("./scaffold-test"), temp_dir_path, &file);
            }
            if found {
                panic!();
            }
        }
    }

    fn scaffold_plugin(current_dir: &Path, dir_path: &Path, plugin_type: &str) -> Result<()> {
        let opts = Opts::builder(PathBuf::from("templates").join("plugin"))
            .project_name(plugin_type)
            .target_dir(dir_path)
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

    fn diff_file(left_folder: &Path, right_folder: &Path, file: &Path) {
        println!("file changed: {}\n", file.to_str().unwrap());
        let left = std::fs::read_to_string(left_folder.join(file)).unwrap();
        let right = std::fs::read_to_string(right_folder.join(file)).unwrap();

        let diff = TextDiff::from_lines(&left, &right);

        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            print!(
                "{} {}|\t{}{}",
                change
                    .old_index()
                    .map(|s| s.to_string())
                    .unwrap_or("-".to_string()),
                change
                    .new_index()
                    .map(|s| s.to_string())
                    .unwrap_or("-".to_string()),
                sign,
                change
            );
        }
        println!("\n\n");
    }
}
