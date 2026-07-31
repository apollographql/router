use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

use bytesize::ByteSize;
use sha2::Digest;

use super::config::WasmConfig;
use super::config::WasmHook;
use super::config::WasmHookConfig;
use super::config::WasmHookSelector;
use super::config::WasmNameMatcher;
use super::config::WasmPermissions;
use super::config::WasmSource;
use super::hooks::apply_header_mutations;
use super::runtime::load_source;
use super::wit;

#[test]
fn plugin_limit_overrides_inherit_configured_defaults() {
    let config: WasmConfig = serde_yaml::from_str(
        r#"
defaults:
  limits:
    execution_timeout: 40ms
    max_memory_per_instance: 24MiB
plugins:
  - name: policy
    source:
      type: file
      path: policy.wasm
    hooks:
      - hook: supergraph.request
    limits:
      execution_timeout: 5ms
"#,
    )
    .expect("valid configuration");

    let limits = config.plugins[0]
        .limits
        .clone()
        .apply_to(config.defaults.limits);
    assert_eq!(limits.execution_timeout, Duration::from_millis(5));
    assert_eq!(limits.max_memory_per_instance, ByteSize::mib(24));
    assert_eq!(limits.max_concurrency, 128);
}

#[test]
fn config_has_no_version_or_verification_policy() {
    assert!(serde_yaml::from_str::<WasmConfig>("version: 1").is_err());
    assert!(serde_yaml::from_str::<WasmConfig>("verification: required").is_err());

    let config: WasmConfig = serde_yaml::from_str(
        r#"
plugins:
  - name: policy
    source:
      type: file
      path: policy.wasm
      digest: sha256:abc
    configuration:
      policy_name: checkout
    hooks:
      - hook: supergraph.request
"#,
    )
    .expect("valid configuration");
    let WasmSource::File { digest, .. } = &config.plugins[0].source;
    assert_eq!(digest.as_deref(), Some("sha256:abc"));
    assert_eq!(config.plugins[0].configuration["policy_name"], "checkout");
}

#[test]
fn names_are_case_insensitive() {
    let matcher = WasmNameMatcher {
        names: HashSet::from(["Authorization".to_string()]),
    };
    assert!(matcher.contains("authorization"));
    assert!(matcher.contains("AUTHORIZATION"));
    assert!(!matcher.contains("x-other"));
}

#[test]
fn source_digest_is_verified() {
    let mut file = tempfile::NamedTempFile::new().expect("temporary file");
    file.write_all(b"component").expect("write component");
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(b"component"));
    let source = WasmSource::File {
        path: file.path().to_path_buf(),
        digest: Some(digest),
    };
    assert_eq!(load_source("test", &source).unwrap(), b"component");

    let invalid = WasmSource::File {
        path: file.path().to_path_buf(),
        digest: Some("sha256:invalid".to_string()),
    };
    assert!(load_source("test", &invalid).is_err());
}

#[test]
fn unauthorized_header_mutation_is_rejected() {
    let mut headers = http::HeaderMap::new();
    let result = apply_header_mutations(
        &mut headers,
        &WasmNameMatcher::default(),
        vec![wit::HeaderOperation::Set(wit::Header {
            name: "x-not-allowed".to_string(),
            values: vec!["value".to_string()],
        })],
    );
    assert!(result.is_err());
    assert!(headers.is_empty());
}

#[test]
fn empty_selector_matches_every_service() {
    let selector = WasmHookSelector::default();
    assert!(selector.matches_service("products"));

    let selector = WasmHookSelector {
        service_names: HashSet::from(["inventory".to_string()]),
    };
    assert!(selector.matches_service("inventory"));
    assert!(!selector.matches_service("products"));
}

#[test]
fn supergraph_hooks_reject_service_selectors() {
    let hook = WasmHookConfig {
        hook: WasmHook::SupergraphRequest,
        selector: WasmHookSelector {
            service_names: HashSet::from(["products".to_string()]),
        },
        permissions: WasmPermissions::default(),
        failure: None,
    };

    assert_eq!(
        hook.validate("policy").unwrap_err(),
        "wasm plugin `policy` cannot select services for `supergraph.request`"
    );
}
