// Simple integration test to verify the derive macro generates working code  
use apollo_router_error::{Error as RouterError, Error};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum TestError {
    #[error("Basic error occurred")]
    #[diagnostic(code(apollo_router::test::basic_error))]
    BasicError,

    #[error("Config error: {message}")]
    #[diagnostic(code(apollo_router::test::config_error), help("Check your config"))]
        ConfigError {
        #[extension("messageString")]
        message: String,
        #[extension]
        line: u32,
    },
    
    #[error("Network error for endpoint: {endpoint}")]
    #[diagnostic(code(apollo_router::test::network_error))]
    NetworkError {
        #[extension]
        endpoint: String,
        #[source]
        io_error: std::io::Error, // This should be excluded from GraphQL extensions
    },
}

#[test]
fn test_error_code_implementation() {
    let basic_error = TestError::BasicError;
    let config_error = TestError::ConfigError {
        message: "test".to_string(),
        line: 42,
    };
    let network_error = TestError::NetworkError {
        endpoint: "http://example.com".to_string(),
        io_error: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused"),
    };

    assert_eq!(basic_error.error_code(), "apollo_router::test::basic_error");
    assert_eq!(
        config_error.error_code(),
        "apollo_router::test::config_error"
    );
    assert_eq!(
        network_error.error_code(),
        "apollo_router::test::network_error"
    );
}

#[test]
fn test_graphql_extensions() {
    let config_error = TestError::ConfigError {
        message: "invalid port".to_string(),
        line: 5,
    };

    let network_error = TestError::NetworkError {
        endpoint: "http://example.com".to_string(),
        io_error: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused"),
    };

    // Test config error extensions
    let mut details = HashMap::new();
    config_error.populate_graphql_extensions(&mut details);

    assert_eq!(
        details.get("errorType").unwrap(),
        &serde_json::Value::String("config".to_string())
    );
    assert_eq!(
        details.get("messageString").unwrap(),
        &serde_json::Value::String("invalid port".to_string())
    );
    assert_eq!(
        details.get("line").unwrap(),
        &serde_json::Value::Number(5.into())
    );

    // Test network error extensions
    details.clear();
    network_error.populate_graphql_extensions(&mut details);

    assert_eq!(
        details.get("errorType").unwrap(),
        &serde_json::Value::String("network".to_string())
    );
    assert_eq!(
        details.get("endpoint").unwrap(),
        &serde_json::Value::String("http://example.com".to_string())
    );
    // io_error should be excluded because it has #[source] attribute
    assert!(!details.contains_key("io_error"));
}
