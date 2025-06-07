use miette::SourceSpan;

// Re-export the derive macro for user convenience
// Re-export error trait and GraphQL types from apollo-router-error
pub use apollo_router_error::{
    Error as RouterError, GraphQLError, GraphQLErrorContext, GraphQLErrorContextBuilder,
    GraphQLErrorExtensions, GraphQLErrorLocation, GraphQLPathSegment,
};



// Re-export error registry functions for introspection
use apollo_router_error::Error;
pub use apollo_router_error::{
    ErrorRegistryEntry, ErrorStats, ErrorVariantInfo, export_error_registry_json,
    get_all_error_codes, get_error_by_code, get_error_by_variant_code, get_error_stats,
    get_errors_by_category, get_errors_by_component, get_registered_errors,
};

/// Core error type for Apollo Router services with comprehensive error codes
///
/// Demonstrates usage of the Error derive macro which automatically:
/// - Implements the Error trait
/// - Registers errors with the global error registry
/// - Generates GraphQL extensions population code
#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum CoreError {
    /// HTTP server configuration error
    #[error("HTTP server configuration failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_HTTP_SERVER_CONFIG_ERROR),
        help("Check your HTTP server configuration parameters")
    )]
    HttpServerConfig {
        message: String,
        #[source_code]
        config_source: Option<String>,
        #[label("Configuration error occurred here")]
        error_span: Option<SourceSpan>,
    },

    /// Query parsing failure
    #[error("GraphQL query parsing failed: {reason}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR),
        help("Ensure your GraphQL query syntax is correct")
    )]
    QueryParseSyntax {
        reason: String,
        #[source_code]
        query_source: Option<String>,
        #[label("Parse error")]
        error_span: Option<SourceSpan>,
    },

    /// Query planning failure
    #[error("Query planning failed: {reason}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_PLANNING_ERROR),
        help("Check your schema federation setup")
    )]
    QueryPlanningFailed {
        reason: String,
        operation_name: Option<String>,
    },

    /// Service execution timeout
    #[error("Service execution timed out after {timeout_ms}ms")]
    #[diagnostic(
        code(APOLLO_ROUTER_EXECUTION_TIMEOUT),
        help("Consider increasing timeout limits or optimizing your resolvers")
    )]
    ExecutionTimeout {
        timeout_ms: u64,
        service_name: String,
    },

    /// Extension loading failure
    #[error("Failed to load extension: {extension_name}")]
    #[diagnostic(
        code(APOLLO_ROUTER_EXTENSIONS_LOAD_ERROR),
        help("Verify the extension is properly configured and available")
    )]
    ExtensionLoadError {
        extension_name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// JSON serialization/deserialization error
    #[error("JSON operation failed")]
    #[diagnostic(code(APOLLO_ROUTER_JSON_OPERATION_ERROR))]
    JsonError(#[from] serde_json::Error),

    /// Network communication error
    #[error("Network communication failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_NETWORK_COMMUNICATION_ERROR),
        help("Check network connectivity and service endpoints")
    )]
    NetworkError(#[from] std::io::Error),
}

/// Layer-specific errors for transformation operations
#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum LayerError {
    /// HTTP to bytes transformation failed
    #[error("HTTP to bytes conversion failed: {details}")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_HTTP_TO_BYTES_CONVERSION_ERROR),
        help("Check that the HTTP request body is valid")
    )]
    HttpToBytesConversion {
        details: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Bytes to JSON transformation failed
    #[error("Bytes to JSON conversion failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_TO_JSON_CONVERSION_ERROR),
        help("Ensure the input is valid JSON")
    )]
    BytesToJsonConversion {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        input_data: Option<String>,
        #[label("Invalid JSON")]
        error_position: Option<SourceSpan>,
    },

    /// Service composition error
    #[error("Service composition failed during {phase}")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_COMPOSITION_SERVICE_ERROR),
        help("Check service configuration and dependencies")
    )]
    ServiceComposition {
        phase: String,
        service_name: String,
        #[source]
        underlying_error: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Result type alias for Apollo Router Core operations
pub type Result<T> = std::result::Result<T, CoreError>;

/// Result type alias for Layer operations
pub type LayerResult<T> = std::result::Result<T, LayerError>;

#[cfg(test)]
mod tests {
    use super::*;
    use miette::NamedSource;

    #[test]
    fn test_error_codes_are_machine_readable() {
        let error = CoreError::QueryParseSyntax {
            reason: "Missing closing brace".to_string(),
            query_source: Some("query { user { name }".to_string()),
            error_span: Some((20, 1).into()),
        };

        assert_eq!(
            error.error_code(),
            "APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR"
        );
    }

    #[test]
    fn test_error_with_source_code_spans() {
        let query = "query { user { name }"; // Missing closing brace
        let error = CoreError::QueryParseSyntax {
            reason: "Missing closing brace".to_string(),
            query_source: Some(query.to_string()),
            error_span: Some((20, 1).into()),
        };

        // Error should include source code information for rich diagnostics
        assert!(matches!(
            error,
            CoreError::QueryParseSyntax {
                query_source: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn test_layered_error_conversion() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();

        let layer_err = LayerError::BytesToJsonConversion {
            json_error: json_err,
            input_data: Some("invalid json".to_string()),
            error_position: Some((0, 7).into()),
        };

        assert_eq!(
            layer_err.error_code(),
            "APOLLO_ROUTER_LAYERS_BYTES_TO_JSON_CONVERSION_ERROR"
        );
    }

    #[test]
    fn test_graphql_error_conversion_basic() {
        let error = CoreError::QueryPlanningFailed {
            reason: "Schema not found".to_string(),
            operation_name: Some("GetUser".to_string()),
        };

        let graphql_error = error.to_graphql_error();

        assert_eq!(
            graphql_error.message,
            "Query planning failed: Schema not found"
        );
        assert_eq!(
            graphql_error.extensions.code,
            "APOLLO_ROUTER_QUERY_PLAN_PLANNING_ERROR"
        );
        assert_eq!(graphql_error.extensions.service, "apollo-router");
        assert!(graphql_error.locations.is_empty());
        assert!(graphql_error.path.is_none());

        // Check error-specific details
        let details = &graphql_error.extensions.details;
        assert_eq!(
            details.get("errorType").unwrap(),
            &serde_json::Value::String("QUERY_PLANNING_FAILED".to_string())
        );
    }

    #[test]
    fn test_graphql_error_conversion_with_context() {
        let error = CoreError::ExecutionTimeout {
            timeout_ms: 5000,
            service_name: "user-service".to_string(),
        };

        let context = GraphQLErrorContext::builder()
            .service_name("query-execution")
            .trace_id("trace-123")
            .request_id("req-456")
            .location(3, 15)
            .path_field("user")
            .path_field("profile")
            .path_index(0)
            .build();

        let graphql_error = error.to_graphql_error_with_context(context);

        assert_eq!(graphql_error.extensions.service, "query-execution");
        assert_eq!(
            graphql_error.extensions.trace_id,
            Some("trace-123".to_string())
        );
        assert_eq!(
            graphql_error.extensions.request_id,
            Some("req-456".to_string())
        );

        // Check locations
        assert_eq!(graphql_error.locations.len(), 1);
        assert_eq!(graphql_error.locations[0].line, 3);
        assert_eq!(graphql_error.locations[0].column, 15);

        // Check path
        let path = graphql_error.path.unwrap();
        assert_eq!(path.len(), 3);
        assert!(matches!(path[0], GraphQLPathSegment::Field(ref s) if s == "user"));
        assert!(matches!(path[1], GraphQLPathSegment::Field(ref s) if s == "profile"));
        assert!(matches!(path[2], GraphQLPathSegment::Index(0)));
    }

    #[test]
    fn test_layer_error_graphql_conversion() {
        let json_err = serde_json::from_str::<serde_json::Value>("{ invalid json").unwrap_err();

        let layer_err = LayerError::BytesToJsonConversion {
            json_error: json_err,
            input_data: Some("{ invalid json data".to_string()),
            error_position: Some((2, 5).into()),
        };

        let graphql_error = layer_err.to_graphql_error();

        assert_eq!(
            graphql_error.extensions.code,
            "APOLLO_ROUTER_LAYERS_BYTES_TO_JSON_CONVERSION_ERROR"
        );

        // Check layer-specific details
        let details = &graphql_error.extensions.details;
        assert_eq!(
            details.get("errorType").unwrap(),
            &serde_json::Value::String("BYTES_TO_JSON_CONVERSION".to_string())
        );
    }

    #[test]
    fn test_graphql_error_json_serialization() {
        let error = CoreError::HttpServerConfig {
            message: "Invalid port number".to_string(),
            config_source: Some("port: invalid_port".to_string()),
            error_span: Some((6, 12).into()),
        };

        let context = GraphQLErrorContext::builder()
            .service_name("http-server")
            .location(1, 7)
            .build();

        let graphql_error = error.to_graphql_error_with_context(context);

        // Serialize to JSON
        let json_result = serde_json::to_string_pretty(&graphql_error);
        assert!(json_result.is_ok());

        let json_str = json_result.unwrap();
        assert!(json_str.contains("APOLLO_ROUTER_HTTP_SERVER_CONFIG_ERROR"));
        assert!(json_str.contains("Invalid port number"));
        assert!(json_str.contains("http-server"));

        // Deserialize back
        let deserialized: std::result::Result<GraphQLError, _> = serde_json::from_str(&json_str);
        assert!(deserialized.is_ok());

        let deserialized_error = deserialized.unwrap();
        assert_eq!(
            deserialized_error.extensions.code,
            graphql_error.extensions.code
        );
        assert_eq!(deserialized_error.message, graphql_error.message);
    }

    #[test]
    fn test_graphql_context_builder() {
        let context = GraphQLErrorContext::builder()
            .service_name("test-service")
            .trace_id("trace-789")
            .request_id("req-101112")
            .location(5, 20)
            .location(7, 25)
            .path_field("query")
            .path_index(2)
            .path_field("users")
            .build();

        assert_eq!(context.service_name, Some("test-service".to_string()));
        assert_eq!(context.trace_id, Some("trace-789".to_string()));
        assert_eq!(context.request_id, Some("req-101112".to_string()));

        let locations = context.locations.unwrap();
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].line, 5);
        assert_eq!(locations[0].column, 20);
        assert_eq!(locations[1].line, 7);
        assert_eq!(locations[1].column, 25);

        let path = context.path.unwrap();
        assert_eq!(path.len(), 3);
        assert!(matches!(path[0], GraphQLPathSegment::Field(ref s) if s == "query"));
        assert!(matches!(path[1], GraphQLPathSegment::Index(2)));
        assert!(matches!(path[2], GraphQLPathSegment::Field(ref s) if s == "users"));
    }



    #[test]
    fn test_graphql_error_extensions_optional_fields() {
        let error = CoreError::NetworkError(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "Connection refused",
        ));

        let graphql_error = error.to_graphql_error();

        // Verify optional fields are properly serialized/omitted
        let json_str = serde_json::to_string(&graphql_error).unwrap();
        let json_value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // Should not include trace_id or request_id since they weren't provided
        assert!(
            json_value
                .get("extensions")
                .unwrap()
                .get("trace_id")
                .is_none()
        );
        assert!(
            json_value
                .get("extensions")
                .unwrap()
                .get("request_id")
                .is_none()
        );

        // Should not include locations or path since they're empty/None
        assert!(json_value.get("locations").is_none());
        assert!(json_value.get("path").is_none());

        // But should include required fields
        assert!(json_value.get("message").is_some());
        assert!(json_value.get("extensions").unwrap().get("code").is_some());
        assert!(
            json_value
                .get("extensions")
                .unwrap()
                .get("timestamp")
                .is_some()
        );
        assert!(
            json_value
                .get("extensions")
                .unwrap()
                .get("service")
                .is_some()
        );
    }
}

/// Utility macros for creating errors with consistent formatting

/// Create a CoreError with source code context
#[macro_export]
macro_rules! core_error {
    (QueryParseSyntax { reason: $reason:expr, query: $query:expr, span: $span:expr }) => {
        $crate::error::CoreError::QueryParseSyntax {
            reason: $reason.to_string(),
            query_source: Some($query.to_string()),
            error_span: Some($span.into()),
        }
    };

    (HttpServerConfig { message: $msg:expr }) => {
        $crate::error::CoreError::HttpServerConfig {
            message: $msg.to_string(),
            config_source: None,
            error_span: None,
        }
    };
}

/// Create a LayerError with context
#[macro_export]
macro_rules! layer_error {
    (BytesToJsonConversion { error: $err:expr, data: $data:expr }) => {
        $crate::error::LayerError::BytesToJsonConversion {
            json_error: $err,
            input_data: Some($data.to_string()),
            error_position: None,
        }
    };
}
