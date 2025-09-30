//! Tests for the diagnostics plugin

use std::net::SocketAddr;
use std::str::FromStr;

use http::Method;
use http::StatusCode;
use tower::Service;
use tower::ServiceExt;

use super::*;
use crate::plugins::test::PluginTestHarness;

#[tokio::test]
async fn test_diagnostics() {
    // This test verifies that the plugin initializes successfully on all platforms
    let config = Config {
        enabled: true,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: "/tmp/router-diagnostics".to_string(),
    };

    let init = PluginInit {
        config,
        previous_config: None,
        supergraph_sdl: Arc::new("schema".to_string()),
        supergraph_schema_id: Arc::new("id".to_string()),
        supergraph_schema: Arc::new(apollo_compiler::validation::Valid::assume_valid(
            apollo_compiler::Schema::new(),
        )),
        subgraph_schemas: Arc::new(std::collections::HashMap::new()),
        launch_id: None,
        notify: crate::notification::Notify::for_tests(),
        license: Arc::new(crate::uplink::license_enforcement::LicenseState::Unlicensed),
        full_config: None,
        original_yaml: Some(Arc::from("test_config")),
    };

    let result = DiagnosticsPlugin::new(init).await;
    assert!(result.is_ok(), "Plugin should work on all platforms");

    let diagnostics = result.unwrap();
    let endpoints = diagnostics.web_endpoints();
    assert!(
        !endpoints.is_empty(),
        "Should have diagnostic endpoints on all platforms"
    );
}

#[tokio::test]
async fn test_diagnostics_disabled() {
    // This test verifies that disabled diagnostics work on any platform
    let config = Config {
        enabled: false,
        listen: SocketAddr::from_str("127.0.0.1:8089").unwrap().into(),
        output_directory: "/tmp/router-diagnostics".to_string(),
    };

    let init = PluginInit {
        config,
        previous_config: None,
        supergraph_sdl: Arc::new("schema".to_string()),
        supergraph_schema_id: Arc::new("id".to_string()),
        supergraph_schema: Arc::new(apollo_compiler::validation::Valid::assume_valid(
            apollo_compiler::Schema::new(),
        )),
        subgraph_schemas: Arc::new(std::collections::HashMap::new()),
        launch_id: None,
        notify: crate::notification::Notify::for_tests(),
        license: Arc::new(crate::uplink::license_enforcement::LicenseState::Unlicensed),
        full_config: None,
        original_yaml: Some(Arc::from("test_config")),
    };

    let result = DiagnosticsPlugin::new(init).await;
    assert!(
        result.is_ok(),
        "Disabled plugin should work on any platform"
    );
}

#[tokio::test]
async fn test_diagnostics_disabled_by_default() {
    let test_harness: PluginTestHarness<DiagnosticsPlugin> = PluginTestHarness::builder()
        .config(
            r#"
            experimental_diagnostics:
                enabled: false
                output_directory: "/tmp/test-diagnostics"
        "#,
        )
        .build()
        .await
        .expect("test harness");

    let endpoints = test_harness.web_endpoints();
    assert!(endpoints.is_empty());
}

#[tokio::test]
async fn test_diagnostics_enabled_creates_endpoint() {
    let listen_addr = "127.0.0.1:0";

    let test_harness: PluginTestHarness<DiagnosticsPlugin> = PluginTestHarness::builder()
        .config(&format!(
            r#"
            experimental_diagnostics:
                enabled: true
                listen: "{}"
                output_directory: "/tmp/test-diagnostics"
        "#,
            listen_addr
        ))
        .build()
        .await
        .expect("test harness");

    let endpoints = test_harness.web_endpoints();
    let listen_addr_key: ListenAddr = SocketAddr::from_str(listen_addr).unwrap().into();
    let endpoint = endpoints.get(&listen_addr_key);
    assert!(endpoint.is_some(), "No endpoint found for listen address");
    let endpoint = endpoint.unwrap();

    // Test the endpoint responds to requests
    let mut router = endpoint.clone().into_router();
    let mut service = router.as_service();

    let request = http::Request::builder()
        .uri("/diagnostics/memory/status")
        .method(Method::GET)
        .body(http_body_util::Empty::new())
        .expect("valid request");

    let response = service.ready().await.unwrap().call(request).await.unwrap();
    // Should be able to access the endpoint (200 on Linux, 501 Not Implemented on other platforms)
    assert!(response.status().is_success() || response.status() == StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn test_diagnostics_endpoints_accessible() {
    let listen_addr = "127.0.0.1:0";

    let test_harness: PluginTestHarness<DiagnosticsPlugin> = PluginTestHarness::builder()
        .config(&format!(
            r#"
            experimental_diagnostics:
                enabled: true
                listen: "{}"
                output_directory: "/tmp/test-diagnostics"
        "#,
            listen_addr
        ))
        .build()
        .await
        .expect("test harness");

    let endpoints = test_harness.web_endpoints();
    let listen_addr_key: ListenAddr = SocketAddr::from_str(listen_addr).unwrap().into();
    let endpoint = endpoints.get(&listen_addr_key).unwrap();

    let mut router = endpoint.clone().into_router();
    let mut service = router.as_service();

    // Test memory status endpoint
    let request = http::Request::builder()
        .uri("/diagnostics/memory/status")
        .method(Method::GET)
        .body(http_body_util::Empty::new())
        .expect("valid request");

    let response = service.ready().await.unwrap().call(request).await.unwrap();
    // Should be able to access the endpoint now (200 on Linux, 501 Not Implemented on other platforms)
    assert!(response.status().is_success() || response.status() == StatusCode::NOT_IMPLEMENTED);

    // Test invalid endpoint returns 404
    let request = http::Request::builder()
        .uri("/diagnostics/invalid")
        .method(Method::GET)
        .body(http_body_util::Empty::new())
        .expect("valid request");

    let response = service.ready().await.unwrap().call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
