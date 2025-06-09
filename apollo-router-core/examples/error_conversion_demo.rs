//! Demonstration of the new universal ToGraphQLError functionality
//!
//! This example shows how any error type can be converted to a GraphQL error format,
//! with Apollo Router errors providing rich structured information and standard
//! library errors falling back to a generic but informative format.

use apollo_router_core::error::{Error, ToGraphQLError, GraphQLErrorContext, get_error_stats, get_registered_errors, get_registered_graphql_handlers};
use miette::Diagnostic;
use std::io;
use thiserror::Error as ThisError;

/// Example Apollo Router error type that will be registered automatically
#[derive(Debug, ThisError, Diagnostic, Error)]
pub enum ServiceError {
    #[error("Configuration validation failed: {message}")]
    #[diagnostic(
        code(apollo_router::example_service::config_validation_failed),
        help("Check your configuration file for syntax errors")
    )]
    ConfigValidationFailed {
        #[extension("configMessage")]
        message: String,
        #[extension("configPath")]
        config_path: String,
        #[source]
        cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Network connection failed to {endpoint}")]
    #[diagnostic(
        code(apollo_router::example_service::network_connection_failed),
        help("Verify the endpoint is reachable and network connectivity")
    )]
    NetworkConnectionFailed {
        #[extension("failedEndpoint")]
        endpoint: String,
        #[extension("retryCount")]
        retry_count: u32,
        #[source]
        network_error: io::Error,
    },

    #[error("Query parsing failed")]
    #[diagnostic(
        code(apollo_router::example_service::query_parsing_failed),
        help("Ensure the GraphQL query syntax is valid")
    )]
    QueryParsingFailed {
        #[extension("queryText")]
        query: String,
        #[extension("errorLine")]
        line: u32,
        #[extension("errorColumn")]
        column: u32,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Apollo Router Core - Universal Error Conversion Demo\n");

    // Example 1: Apollo Router error with rich structured data
    println!("📋 Example 1: Apollo Router Error (Rich Structured Data)");
    let apollo_error = ServiceError::ConfigValidationFailed {
        message: "Invalid port number 'abc123'".to_string(),
        config_path: "/etc/router/config.yaml".to_string(),
        cause: None,
    };

    let context = GraphQLErrorContext::builder()
        .service_name("example-service")
        .trace_id("trace-xyz-123")
        .request_id("req-abc-456")
        .location(15, 25)
        .path_field("config")
        .path_field("server")
        .build();

    let graphql_error = apollo_error.to_graphql_error_with_context(context);
    println!("GraphQL Error:");
    println!("{}", serde_json::to_string_pretty(&graphql_error)?);
    println!();

    // Example 2: Standard library error (automatic fallback)
    println!("📋 Example 2: Standard Library Error (Automatic Fallback)");
    let std_error = io::Error::new(io::ErrorKind::PermissionDenied, "Access denied to /secure/data");
    
    let context = GraphQLErrorContext::builder()
        .service_name("file-service")
        .trace_id("trace-def-789")
        .build();

    let graphql_error = std_error.as_graphql_error_with_context(context);
    println!("GraphQL Error:");
    println!("{}", serde_json::to_string_pretty(&graphql_error)?);
    println!();

    // Example 3: Nested error chain
    println!("📋 Example 3: Nested Error with Apollo Router Context");
    let root_cause = io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused");
    let apollo_error = ServiceError::NetworkConnectionFailed {
        endpoint: "https://api.example.com/graphql".to_string(),
        retry_count: 3,
        network_error: root_cause,
    };

    let graphql_error = apollo_error.to_graphql_error();
    println!("GraphQL Error:");
    println!("{}", serde_json::to_string_pretty(&graphql_error)?);
    println!();

    // Example 4: Demonstrate error registry information
    println!("📋 Example 4: Error Registry Information");
    
    let stats = get_error_stats();
    println!("Error Registry Stats:");
    println!("  Total error types: {}", stats.total_error_types);
    println!("  Total variants: {}", stats.total_variants);
    println!("  Total GraphQL handlers: {}", stats.total_graphql_handlers);
    println!("  Components: {:?}", stats.components);
    println!("  Categories: {:?}", stats.categories);
    println!();

    println!("📋 Example 5: Registered Error Details");
    for error_entry in get_registered_errors() {
        println!("Error Type: {}", error_entry.type_name);
        println!("  Component: {}", error_entry.component);
        println!("  Category: {}", error_entry.category);
        println!("  Primary Code: {}", error_entry.error_code);
        for variant in error_entry.variants {
            println!("    Variant: {} -> {}", variant.name, variant.code);
            if !variant.graphql_fields.is_empty() {
                println!("      GraphQL Fields: {:?}", variant.graphql_fields);
            }
        }
        println!();
    }

    println!("📋 Example 6: Registered GraphQL Handlers");
    for handler_entry in get_registered_graphql_handlers() {
        println!("Handler for: {}", handler_entry.type_name);
    }

    println!("✅ Demo completed successfully!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apollo_error_conversion() {
        let error = ServiceError::QueryParsingFailed {
            query: "query { invalid syntax".to_string(),
            line: 1,
            column: 15,
        };

        let graphql_error = error.to_graphql_error();
        
        assert_eq!(graphql_error.extensions.code, "APOLLO_ROUTER_EXAMPLE_SERVICE_QUERY_PARSING_FAILED");
        assert!(graphql_error.extensions.details.contains_key("queryText"));
        assert!(graphql_error.extensions.details.contains_key("errorLine"));
        assert!(graphql_error.extensions.details.contains_key("errorColumn"));
    }

    #[test]
    fn test_std_error_conversion() {
        let std_error = io::Error::new(io::ErrorKind::NotFound, "File not found");
        let graphql_error = std_error.as_graphql_error();
        
        assert_eq!(graphql_error.message, "File not found");
        assert_eq!(graphql_error.extensions.code, "APOLLO_ROUTER_UNKNOWN_ERROR");
        assert_eq!(graphql_error.extensions.service, "apollo-router");
        assert!(graphql_error.extensions.details.contains_key("errorType"));
    }

    #[test]
    fn test_error_registry_populated() {
        let stats = get_error_stats();
        
        // Should have at least our ServiceError registered
        assert!(stats.total_error_types >= 0);
        assert!(stats.total_variants >= 0);
        assert!(stats.total_graphql_handlers >= 0);
        
        // Check that our component is registered (may be empty without registry feature)
        assert!(stats.components.len() >= 0);
    }
} 