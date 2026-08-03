use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use bytesize::ByteSize;
use sha2::Digest;

use super::config::WasmConfig;
use super::config::WasmHook;
use super::config::WasmHookConfig;
use super::config::WasmHookSelector;
use super::config::WasmNameMatcher;
use super::config::WasmPermissions;
use super::config::WasmSource;
use super::config::WasmTransportAccess;
use super::hooks::apply_connector_mutation;
use super::hooks::apply_header_mutations;
use super::hooks::apply_subgraph_mutation;
use super::hooks::apply_supergraph_mutation;
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
        ..Default::default()
    };
    assert!(selector.matches_service("inventory"));
    assert!(!selector.matches_service("products"));
}

#[test]
fn supergraph_hooks_reject_service_selectors() {
    let hook = WasmHookConfig {
        hook: WasmHook::Supergraph,
        selector: WasmHookSelector {
            service_names: HashSet::from(["products".to_string()]),
            ..Default::default()
        },
        permissions: WasmPermissions::default(),
        failure: None,
    };

    assert_eq!(
        hook.validate("policy").unwrap_err(),
        "wasm plugin `policy` cannot use selectors for `supergraph.request`"
    );
}

#[test]
fn connector_selectors_match_each_identity_dimension() {
    let selector = WasmHookSelector {
        service_names: HashSet::from(["products".to_string()]),
        source_names: HashSet::from(["catalog".to_string()]),
        connector_names: HashSet::from(["product-by-id".to_string()]),
    };

    assert!(selector.matches_connector("products", Some("catalog"), "product-by-id"));
    assert!(!selector.matches_connector("inventory", Some("catalog"), "product-by-id"));
    assert!(!selector.matches_connector("products", None, "product-by-id"));
    assert!(!selector.matches_connector("products", Some("catalog"), "other"));
}

#[test]
fn non_connector_hooks_reject_method_and_uri_mutations() {
    let hook = WasmHookConfig {
        hook: WasmHook::Supergraph,
        selector: Default::default(),
        permissions: Default::default(),
        failure: None,
    };
    let mutation = wit::Mutation {
        headers: Vec::new(),
        context: Vec::new(),
        method: Some("PATCH".to_string()),
        uri: None,
        body: None,
    };
    let mut supergraph_request = crate::services::supergraph::Request::fake_builder()
        .build()
        .unwrap();
    assert!(apply_supergraph_mutation(&mut supergraph_request, &hook, mutation).is_err());

    let hook = WasmHookConfig {
        hook: WasmHook::Subgraph,
        selector: Default::default(),
        permissions: Default::default(),
        failure: None,
    };
    let mutation = wit::Mutation {
        headers: Vec::new(),
        context: Vec::new(),
        method: None,
        uri: Some("/other".to_string()),
        body: None,
    };
    let mut subgraph_request = crate::services::subgraph::Request::fake_builder().build();
    assert!(apply_subgraph_mutation(&mut subgraph_request, &hook, mutation).is_err());
}

#[test]
fn connector_http_mutation_preserves_origin_and_repairs_content_length() {
    let mut permissions = WasmPermissions::default();
    permissions.transport.method = WasmTransportAccess::ReadWrite;
    permissions.transport.uri = WasmTransportAccess::ReadWrite;
    permissions.transport.body = WasmTransportAccess::ReadWrite;
    permissions
        .headers
        .write
        .names
        .insert("x-plugin".to_string());
    let hook = WasmHookConfig {
        hook: WasmHook::Connector,
        selector: Default::default(),
        permissions,
        failure: None,
    };
    let mut request = connector_http_request();
    let mutation = wit::Mutation {
        headers: vec![wit::HeaderOperation::Set(wit::Header {
            name: "x-plugin".to_string(),
            values: vec!["active".to_string()],
        })],
        context: Vec::new(),
        method: Some("POST".to_string()),
        uri: Some("/next?page=2".to_string()),
        body: Some("new body".to_string()),
    };

    apply_connector_mutation(&mut request, &crate::Context::new(), &hook, mutation).unwrap();

    let TransportRequest::Http(request) = request else {
        panic!("expected HTTP request");
    };
    assert_eq!(request.inner.method(), http::Method::POST);
    assert_eq!(request.inner.uri(), "https://example.com/next?page=2");
    assert_eq!(request.inner.body(), "new body");
    assert_eq!(request.inner.headers()["x-plugin"], "active");
    assert!(
        !request
            .inner
            .headers()
            .contains_key(http::header::CONTENT_LENGTH)
    );
}

#[test]
fn connector_mutation_rejects_origin_changes_without_partial_application() {
    let mut permissions = WasmPermissions::default();
    permissions.transport.uri = WasmTransportAccess::ReadWrite;
    permissions
        .headers
        .write
        .names
        .insert("x-plugin".to_string());
    let hook = WasmHookConfig {
        hook: WasmHook::Connector,
        selector: Default::default(),
        permissions,
        failure: None,
    };
    let mut request = connector_http_request();
    let mutation = wit::Mutation {
        headers: vec![wit::HeaderOperation::Set(wit::Header {
            name: "x-plugin".to_string(),
            values: vec!["active".to_string()],
        })],
        context: Vec::new(),
        method: None,
        uri: Some("https://attacker.example/path".to_string()),
        body: None,
    };

    assert!(
        apply_connector_mutation(&mut request, &crate::Context::new(), &hook, mutation).is_err()
    );
    let TransportRequest::Http(request) = request else {
        panic!("expected HTTP request");
    };
    assert_eq!(request.inner.uri(), "https://example.com/original");
    assert!(!request.inner.headers().contains_key("x-plugin"));
}

#[test]
fn mapping_only_connectors_reject_http_mutations() {
    let hook = WasmHookConfig {
        hook: WasmHook::Connector,
        selector: Default::default(),
        permissions: Default::default(),
        failure: None,
    };
    let mut request = TransportRequest::MappingOnly;
    let mutation = wit::Mutation {
        headers: Vec::new(),
        context: Vec::new(),
        method: None,
        uri: None,
        body: Some("not applicable".to_string()),
    };
    assert!(
        apply_connector_mutation(&mut request, &crate::Context::new(), &hook, mutation).is_err()
    );
}

#[test]
fn connector_plugins_cannot_set_content_length() {
    let mut permissions = WasmPermissions::default();
    permissions
        .headers
        .write
        .names
        .insert("content-length".to_string());
    let hook = WasmHookConfig {
        hook: WasmHook::Connector,
        selector: Default::default(),
        permissions,
        failure: None,
    };
    let mut request = connector_http_request();
    let mutation = wit::Mutation {
        headers: vec![wit::HeaderOperation::Set(wit::Header {
            name: "content-length".to_string(),
            values: vec!["999".to_string()],
        })],
        context: Vec::new(),
        method: None,
        uri: None,
        body: None,
    };
    let error = apply_connector_mutation(&mut request, &crate::Context::new(), &hook, mutation)
        .unwrap_err();
    assert!(error.to_string().contains("derived `content-length`"));
}

fn connector_http_request() -> TransportRequest {
    TransportRequest::Http(Box::new(HttpRequest {
        inner: http::Request::builder()
            .method("GET")
            .uri("https://example.com/original")
            .header(http::header::CONTENT_LENGTH, "8")
            .body("original".to_string())
            .unwrap(),
        debug: (None, Vec::new()),
    }))
}
