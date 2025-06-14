//! Example demonstrating the re-exported derive macro
//!
//! This shows that users can now use `apollo_router_error::Error` instead of
//! `apollo_router_error_derive::Error` for convenience.

// use std::collections::HashMap;

use apollo_router_error::Error;

// Using the re-exported derive macro - much cleaner!
#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum MyServiceError {
    #[error("Configuration error: {message}")]
    #[diagnostic(
        code(apollo_router::my_service::config_error),
        help("Check your configuration file")
    )]
    ConfigError {
        #[extension("configMessage")]
        message: String,
        #[extension] // Will use camelCase: "configPath"
        config_path: String,
    },

    #[error("Network timeout after {timeout_ms}ms")]
    #[diagnostic(code(apollo_router::my_service::network_timeout))]
    NetworkTimeout {
        #[extension]
        timeout_ms: u64,
        #[extension("endpoint")]
        endpoint_url: String,
    },
}

fn main() {
    println!("Demonstrating re-exported Error derive macro");

    let config_error = MyServiceError::ConfigError {
        message: "Invalid port number".to_string(),
        config_path: "/etc/router.yaml".to_string(),
    };

    let timeout_error = MyServiceError::NetworkTimeout {
        timeout_ms: 5000,
        endpoint_url: "https://api.example.com".to_string(),
    };

    // Demonstrate error codes
    println!("Config error code: {}", config_error.error_code());
    println!("Timeout error code: {}", timeout_error.error_code());

    // Demonstrate GraphQL extensions
    let mut details = std::collections::BTreeMap::new();
    config_error.populate_graphql_extensions(&mut details);
    println!("Config error extensions: {:#?}", details);

    details.clear();
    timeout_error.populate_graphql_extensions(&mut details);
    println!("Timeout error extensions: {:#?}", details);

    // Demonstrate GraphQL error conversion
    let graphql_error = config_error.to_graphql_error();
    println!("GraphQL error: {:#?}", graphql_error);
}
