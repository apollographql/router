use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use cargo_scaffold::ScaffoldDescription;
use clap::Subcommand;
use inflector::Inflector;
use regex::Regex;
use toml::Value;

#[derive(Subcommand, Debug)]
pub enum PluginAction {
    /// Add a plugin.
    Create {
        /// The name of the plugin you want to add.
        name: String,

        /// Optional override of the scaffold template path.
        #[clap(long)]
        template_override: Option<PathBuf>,
    },

    /// Remove a plugin.
    Remove {
        /// The name of the plugin you want to remove.
        name: String,
    },
}

impl PluginAction {
    pub fn execute(&self) -> Result<()> {
        match self {
            PluginAction::Create {
                name,
                template_override,
            } => create_plugin(name, template_override),
            PluginAction::Remove { name } => remove_plugin(name),
        }
    }
}

fn create_plugin(name: &str, template_path: &Option<PathBuf>) -> Result<()> {
    let plugin_path = plugin_path(name);
    if plugin_path.exists() {
        return Err(anyhow::anyhow!("plugin '{}' already exists", name));
    }

    let cargo_toml = fs::read_to_string("Cargo.toml")?.parse::<toml::Value>()?;
    let project_name = cargo_toml
        .get("package")
        .unwrap_or(&toml::Value::String("default".to_string()))
        .get("name")
        .map(|n| n.to_string().to_snake_case())
        .unwrap_or_else(|| "default".to_string());

    let version = get_router_version(cargo_toml);

    let opts = cargo_scaffold::Opts::builder(template_path.as_ref().unwrap_or(&PathBuf::from(
        "https://github.com/apollographql/router.git",
    )))
    .git_ref(version)
    .repository_template_path(
        PathBuf::from("apollo-router-scaffold")
            .join("templates")
            .join("plugin"),
    )
    .target_dir(".")
    .project_name(name)
    .parameters(vec![format!("name={name}")])
    .append(true);
    let desc = ScaffoldDescription::new(opts)?;
    let mut params = desc.fetch_parameters_value()?;
    params.insert(
        "pascal_name".to_string(),
        Value::String(name.to_pascal_case()),
    );
    params.insert(
        "snake_name".to_string(),
        Value::String(name.to_snake_case()),
    );
    params.insert(
        "project_name".to_string(),
        Value::String(project_name.to_snake_case()),
    );

    params.insert(
        format!(
            "type_{}",
            params
                .get("type")
                .expect("type must have been set")
                .as_str()
                .expect("type must be a string")
        ),
        Value::Boolean(true),
    );

    desc.scaffold_with_parameters(params)?;

    let mod_path = mod_path();
    let mut mod_rs = if mod_path.exists() {
        std::fs::read_to_string(&mod_path)?
    } else {
        "".to_string()
    };

    let snake_name = name.to_snake_case();
    let re = Regex::new(&format!(r"(?m)^mod {snake_name};$")).unwrap();
    if re.find(&mod_rs).is_none() {
        mod_rs = format!("mod {snake_name};\n{mod_rs}");
    }

    std::fs::write(mod_path, mod_rs)?;

    println!(
        "Plugin created at '{}'.\nRemember to add the plugin to your router.yaml to activate it.",
        plugin_path.display()
    );
    Ok(())
}

fn get_router_version(cargo_toml: Value) -> String {
    match cargo_toml
        .get("dependencies")
        .cloned()
        .unwrap_or_else(|| Value::Table(toml::value::Table::default()))
        .get("apollo-router")
    {
        Some(Value::String(version)) => format!("v{version}"),
        Some(Value::Table(table)) => {
            if let Some(Value::String(branch)) = table.get("branch") {
                format!("origin/{}", branch.clone())
            } else if let Some(Value::String(tag)) = table.get("tag") {
                tag.clone()
            } else if let Some(Value::String(rev)) = table.get("rev") {
                rev.clone()
            } else {
                format!("v{}", std::env!("CARGO_PKG_VERSION"))
            }
        }
        _ => format!("v{}", std::env!("CARGO_PKG_VERSION")),
    }
}

fn remove_plugin(name: &str) -> Result<()> {
    let plugin_path = plugin_path(name);
    let snake_name = name.to_snake_case();

    std::fs::remove_file(&plugin_path)?;

    // Remove the mod;
    let mod_path = mod_path();
    if Path::new(&mod_path).exists() {
        let mut mod_rs = std::fs::read_to_string(&mod_path)?;
        let re = Regex::new(&format!(r"(?m)^mod {snake_name};$")).unwrap();
        mod_rs = re.replace(&mod_rs, "").to_string();

        std::fs::write(mod_path, mod_rs)?;
    }

    println!(
        "Plugin removed at '{}'. This is a best effort, and you may need to edit some files manually.",
        plugin_path.display()
    );
    Ok(())
}

fn mod_path() -> PathBuf {
    PathBuf::from("src").join("plugins").join("mod.rs")
}

fn plugin_path(name: &str) -> PathBuf {
    PathBuf::from("src")
        .join("plugins")
        .join(format!("{}.rs", name.to_snake_case()))
}
