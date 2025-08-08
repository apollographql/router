//! Tests for the diagnostics plugin

use std::net::SocketAddr;
use std::str::FromStr;

use http::Method;
use http::StatusCode;
use tower::Service;
use tower::ServiceExt;

use super::*;
use crate::plugins::test::PluginTestHarness;

// Platform-specific tests for non-Linux platforms
#[cfg(not(target_os = "linux"))]
mod platform_tests {
    use super::*;

    #[tokio::test]
    async fn test_diagnostics_fails_on_non_linux_platforms() {
        // This test verifies that the plugin fails to initialize on non-Linux platforms
        let config = Config {
            enabled: true,
            listen: default_diagnostics_listen(),
            shared_secret: "test-secret".to_string(),
            output_directory: default_output_directory(),
        };

        let init = PluginInit {
            config,
            supergraph_sdl: std::sync::Arc::new("schema".to_string()),
            supergraph_schema_id: std::sync::Arc::new("id".to_string()),
            supergraph_schema: std::sync::Arc::new(
                apollo_compiler::validation::Valid::assume_valid(apollo_compiler::Schema::new()),
            ),
            subgraph_schemas: std::sync::Arc::new(std::collections::HashMap::new()),
            launch_id: None,
            notify: crate::notification::Notify::for_tests(),
            license: crate::uplink::license_enforcement::LicenseState::Unlicensed,
            full_config: None,
        };

        let result = Diagnostics::new(init).await;
        assert!(result.is_err(), "Plugin should fail on non-Linux platforms");
        let error_message = result.unwrap_err().to_string();
        assert!(error_message.contains("not supported on this platform"));
        assert!(error_message.contains("Linux-specific jemalloc features"));
    }

    #[tokio::test] 
    async fn test_diagnostics_disabled_works_on_any_platform() {
        // This test verifies that disabled diagnostics work on any platform
        let config = Config {
            enabled: false,
            listen: default_diagnostics_listen(),
            shared_secret: "test-secret".to_string(),
            output_directory: default_output_directory(),
        };

        let init = PluginInit {
            config,
            supergraph_sdl: std::sync::Arc::new("schema".to_string()),
            supergraph_schema_id: std::sync::Arc::new("id".to_string()),
            supergraph_schema: std::sync::Arc::new(
                apollo_compiler::validation::Valid::assume_valid(apollo_compiler::Schema::new()),
            ),
            subgraph_schemas: std::sync::Arc::new(std::collections::HashMap::new()),
            launch_id: None,
            notify: crate::notification::Notify::for_tests(),
            license: crate::uplink::license_enforcement::LicenseState::Unlicensed,
            full_config: None,
        };

        let result = Diagnostics::new(init).await;
        assert!(result.is_ok(), "Disabled plugin should work on any platform");
    }
}

// Linux-specific tests
#[cfg(target_os = "linux")]
mod linux_tests {
    use super::*;

    #[tokio::test]
    async fn test_diagnostics_disabled_by_default() {
        let test_harness: PluginTestHarness<Diagnostics> = PluginTestHarness::builder()
            .config(
                r#"
                experimental_diagnostics:
                    enabled: false
                    shared_secret: "test-secret"
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
    async fn test_diagnostics_requires_shared_secret() {
        // This test verifies that enabling diagnostics without a shared secret fails
        // We can't use PluginTestHarness here due to trait constraints, but the
        // actual validation happens in the plugin's new() method
        let config = Config {
            enabled: true,
            listen: default_diagnostics_listen(),
            shared_secret: String::new(),
            output_directory: default_output_directory(),
        };

        let init = PluginInit {
            config,
            supergraph_sdl: std::sync::Arc::new("schema".to_string()),
            supergraph_schema_id: std::sync::Arc::new("id".to_string()),
            supergraph_schema: std::sync::Arc::new(
                apollo_compiler::validation::Valid::assume_valid(apollo_compiler::Schema::new()),
            ),
            subgraph_schemas: std::sync::Arc::new(std::collections::HashMap::new()),
            launch_id: None,
            notify: crate::notification::Notify::for_tests(),
            license: crate::uplink::license_enforcement::LicenseState::Unlicensed,
            full_config: None,
        };

        let result = Diagnostics::new(init).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("shared_secret"));
    }

    #[tokio::test]
    async fn test_diagnostics_enabled_creates_endpoint() {
        let listen_addr = "127.0.0.1:0";

        let test_harness: PluginTestHarness<Diagnostics> = PluginTestHarness::builder()
            .config(&format!(
                r#"
                experimental_diagnostics:
                    enabled: true
                    listen: "{}"
                    shared_secret: "test-secret-123"
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

        // Test the endpoint responds with authentication error for missing auth
        let mut router = endpoint.clone().into_router();
        let mut service = router.as_service();

        let request = http::Request::builder()
            .uri("/diagnostics/memory/status")
            .method(Method::GET)
            .body(http_body_util::Empty::new())
            .expect("valid request");

        let response = service.ready().await.unwrap().call(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_diagnostics_with_valid_auth() {
        let listen_addr = "127.0.0.1:0";

        let test_harness: PluginTestHarness<Diagnostics> = PluginTestHarness::builder()
            .config(&format!(
                r#"
                experimental_diagnostics:
                    enabled: true
                    listen: "{}"
                    shared_secret: "test-secret-123"
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

        let request = http::Request::builder()
            .uri("/diagnostics/memory/status")
            .method(Method::GET)
            .header("authorization", "Bearer dGVzdC1zZWNyZXQtMTIz") // base64("test-secret-123")
            .body(http_body_util::Empty::new())
            .expect("valid request");

        let response = service.ready().await.unwrap().call(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_diagnostics_export_endpoint() {
        let listen_addr = "127.0.0.1:0";

        // Create a small test file in the memory subdirectory
        let test_dir = "/tmp/test-diagnostics-export";
        let memory_dir = format!("{}/memory", test_dir);
        std::fs::create_dir_all(&memory_dir).ok();
        std::fs::write(format!("{}/test.prof", memory_dir), b"test profile data").ok();

        let test_harness: PluginTestHarness<Diagnostics> = PluginTestHarness::builder()
            .config(&format!(
                r#"
                experimental_diagnostics:
                    enabled: true
                    listen: "{}"
                    shared_secret: "test-secret-123"
                    output_directory: "{}"
            "#,
                listen_addr, test_dir
            ))
            .build()
            .await
            .expect("test harness");

        let endpoints = test_harness.web_endpoints();
        let listen_addr_key: ListenAddr = SocketAddr::from_str(listen_addr).unwrap().into();
        let endpoint = endpoints.get(&listen_addr_key).unwrap();

        let mut router = endpoint.clone().into_router();
        let mut service = router.as_service();

        let request = http::Request::builder()
            .uri("/diagnostics/export")
            .method(Method::GET)
            .header("authorization", "Bearer dGVzdC1zZWNyZXQtMTIz") // base64("test-secret-123")
            .body(http_body_util::Empty::new())
            .expect("valid request");

        let response = service.ready().await.unwrap().call(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        // Check that it's a gzip response
        let content_type = response.headers().get("content-type").unwrap();
        assert_eq!(content_type, "application/gzip");
        
        // Check that it has content-disposition header
        let content_disposition = response.headers().get("content-disposition").unwrap();
        assert!(content_disposition.to_str().unwrap().contains("router-diagnostics-"));
        assert!(content_disposition.to_str().unwrap().contains(".tar.gz"));

        // Cleanup
        std::fs::remove_dir_all(test_dir).ok();
    }
}