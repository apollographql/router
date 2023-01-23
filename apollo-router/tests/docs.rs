use inflector::Inflector;
use serde_json::Value;
use std::path::Path;
#[test]
#[cfg(target_family = "unix")]
fn check_config_json() {
    // Sanity check consistency and that files exist
    let config = serde_json::from_str(include_str!("../../docs/source/config.json"))
        .expect("docs json must be valid");
    let result = jsonpath_lib::select(&config, "$.sidebar..*").expect("values must be selectable");
    for value in result {
        if let Value::String(path) = value {
            if !path.starts_with("https://") {
                assert!(
                    path.replace("/", "").is_kebab_case(),
                    "{} in config.json was not kebab case",
                    path
                );
                if path != "/" {
                    let path_in_docs = format!("../docs/source{}.mdx", path);
                    let path_in_docs = Path::new(&path_in_docs);
                    assert!(
                        path_in_docs.exists(),
                        "{} in docs/source/config.json did not exist",
                        path
                    );
                }
            }
        }
    }
}
