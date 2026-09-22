//! Finds router configuration fixtures, examples and YAML snippets in documentation.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use regex::Regex;
use walkdir::DirEntry;
use walkdir::WalkDir;

/// One YAML document pulled from an integration fixture, an example file, or a fenced code
/// block inside a `.mdx` docs page.
pub(crate) struct DiscoveredConfig {
    /// The file the document came from, for naming in a test failure.
    pub(crate) path: PathBuf,
    pub(crate) yaml: String,
}

/// Walks `.`, `../examples`, `../docs` and `../dockerfiles` for `router.yaml` / `*.router.yaml`
/// files and, on Unix, `router_unix.yaml` / `*.router_unix.yaml` files, plus
/// ```` ```yaml title="router.yaml" ```` (or
/// `title="router_unix.yaml"` on Unix) blocks inside `.mdx` docs. A block with extra attributes
/// after the title, such as `novalidate`, is intentionally invalid or version-specific and is not
/// discovered. A sibling `.skipconfigvalidation` marker file excludes a path from discovery
/// entirely.
pub(crate) fn discover_project_configs() -> Vec<DiscoveredConfig> {
    #[cfg(not(unix))]
    let filename_matcher = Regex::from_str("((.+[.])?router\\.yaml)|(.+\\.mdx)").unwrap();
    #[cfg(unix)]
    let filename_matcher = Regex::from_str("((.+[.])?router(_unix)?\\.yaml)|(.+\\.mdx)").unwrap();
    #[cfg(not(unix))]
    let embedded_yaml_matcher =
        Regex::from_str(r#"(?ms)```yaml title="router.yaml"\n(.+?)```"#).unwrap();
    #[cfg(unix)]
    let embedded_yaml_matcher =
        Regex::from_str(r#"(?ms)```yaml title="router(_unix)?.yaml"\n(.+?)```"#).unwrap();

    fn it(path: &str) -> impl Iterator<Item = DirEntry> + use<> {
        WalkDir::new(path).into_iter().filter_map(|e| e.ok())
    }

    let mut discovered = Vec::new();
    for entry in it(".")
        .chain(it("../examples"))
        .chain(it("../docs"))
        .chain(it("../dockerfiles"))
    {
        if entry
            .path()
            .with_file_name(".skipconfigvalidation")
            .exists()
        {
            continue;
        }
        #[cfg(not(feature = "telemetry_next"))]
        if entry.path().to_string_lossy().contains("telemetry_next") {
            continue;
        }

        let name = entry.file_name().to_string_lossy();
        if !filename_matcher.is_match(&name) {
            continue;
        }

        let config = fs::read_to_string(entry.path()).expect("failed to read file");
        let yamls: Vec<String> = if name.ends_with(".mdx") {
            #[cfg(unix)]
            let index = 2usize;
            #[cfg(not(unix))]
            let index = 1usize;
            embedded_yaml_matcher
                .captures_iter(&config)
                .map(|i| i.get(index).unwrap().as_str().into())
                .collect()
        } else {
            vec![config]
        };

        for yaml in yamls {
            discovered.push(DiscoveredConfig {
                path: entry.path().to_path_buf(),
                yaml,
            });
        }
    }
    discovered
}

/// Synthetic values for the discovered documents' environment-variable placeholders.
pub(crate) fn discovery_env_vars() -> HashMap<String, String> {
    [
        ("DATADOG_AGENT_HOST", "http://example.com"),
        ("JAEGER_HOST", "http://example.com"),
        ("JAEGER_USERNAME", "username"),
        ("JAEGER_PASSWORD", "pass"),
        ("REDIS_USERNAME", "username"),
        ("REDIS_PASSWORD", "pass"),
        ("ZIPKIN_HOST", "http://example.com"),
        ("TEST_CONFIG_ENDPOINT", "http://example.com"),
        ("TEST_CONFIG_COLLECTOR_ENDPOINT", "http://example.com"),
        ("PARSER_MAX_RECURSION", "500"),
        ("AWS_ROLE_ARN", "arn:aws:iam::12345678:role/SomeRole"),
        ("INVALIDATION_SHARED_KEY", "invalidation"),
        (
            "INVALIDATION_SHARED_KEY_PRODUCTS",
            "invalidation-for-products",
        ),
        ("DISTRIBUTED_TRACING_ENDPOINT", "http://example.com"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect()
}
