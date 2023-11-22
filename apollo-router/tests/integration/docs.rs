#[test]
#[cfg(target_family = "unix")]
fn check_config_json() {
    use std::path::Path;

    use regex::Regex;
    use serde_json::Value;

    // Sanity check consistency and that files exist
    let config = serde_json::from_str(include_str!("../../../docs/source/config.json"))
        .expect("docs json must be valid");
    let result = jsonpath_lib::select(&config, "$.sidebar.*.*").expect("values must be selectable");
    let re = Regex::new(r"^[a-z/-]+$").expect("regex must be valid");
    for value in result {
        if let Value::String(path) = value {
            if !path.starts_with("https://") {
                assert!(
                    re.is_match(path),
                    "{path} in config.json was not kebab case"
                );
                if path != "/" {
                    let path_in_docs = format!("../docs/source{path}.mdx");
                    let path_in_docs = Path::new(&path_in_docs);
                    assert!(
                        path_in_docs.exists(),
                        "{path} in docs/source/config.json did not exist"
                    );
                }
            }
        }
    }
}
