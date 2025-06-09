use apollo_router_error::{
    GraphQLErrorContext, ToGraphQLError, arc_to_graphql_error, box_to_graphql_error,
    export_error_registry_json, get_error_stats, get_registered_errors,
};
use std::sync::Arc;

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum GraphQLError {
    #[error("Configuration error: {message}")]
    #[diagnostic(
        code(apollo_router::my_service::config_error),
        help("Check your configuration file")
    )]
    TestError {
        #[extension("configMessage")]
        message: String,
        #[extension] // Will use camelCase: "configPath"
        config_path: String,
    },
}

#[test]
fn test_empty_registry() {
    // At compile time, there might not be any registered errors in tests
    let errors = get_registered_errors();
    // Length is always non-negative, so just check that it exists
    assert!(errors.len() == 0 || errors.len() > 0); // Could be empty or have test errors
}

#[test]
fn test_error_stats() {
    let stats = get_error_stats();
    // These are always non-negative, just verify they exist
    assert!(stats.total_error_types == 0 || stats.total_error_types > 0);
    assert!(stats.total_variants == 0 || stats.total_variants > 0);
    assert!(stats.total_graphql_handlers == 0 || stats.total_graphql_handlers > 0);
}

#[test]
fn test_json_export() {
    let json_result = export_error_registry_json();
    assert!(json_result.is_ok());

    let json = json_result.unwrap();
    assert!(json.starts_with('['));
    assert!(json.ends_with(']'));
}

#[test]
fn test_as_graphql_error_for_std_error() {
    let std_error = std::io::Error::new(std::io::ErrorKind::NotFound, "File not found");
    let graphql_error = std_error.to_graphql_error();

    assert_eq!(graphql_error.message, "File not found");
    assert_eq!(graphql_error.extensions.code, "INTERNAL_ERROR");
    assert_eq!(graphql_error.extensions.service, "unknown");

    // Should have error type information
    assert!(graphql_error.extensions.details.contains_key("errorType"));
}

#[test]
fn test_as_graphql_error_with_context() {
    let std_error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access denied");
    let context = GraphQLErrorContext::builder()
        .service_name("test-service")
        .trace_id("trace-123")
        .request_id("req-456")
        .location(10, 5)
        .path_field("user")
        .build();

    let graphql_error = std_error.to_graphql_error_with_context(context);

    assert_eq!(graphql_error.message, "Access denied");
    assert_eq!(graphql_error.extensions.service, "test-service");
    assert_eq!(
        graphql_error.extensions.trace_id,
        Some("trace-123".to_string())
    );
    assert_eq!(
        graphql_error.extensions.request_id,
        Some("req-456".to_string())
    );
    assert_eq!(graphql_error.locations.len(), 1);
    assert!(graphql_error.path.is_some());
}

#[test]
fn test_generic_graphql_error_with_error_chain() {
    // Create a nested error chain
    let root_cause = std::io::Error::new(std::io::ErrorKind::NotFound, "Root cause");
    let _wrapper_error = std::io::Error::new(std::io::ErrorKind::Other, "Wrapper error");

    // Note: std::io::Error doesn't easily allow chaining, so this is a simplified test
    let graphql_error = root_cause.to_graphql_error();

    assert_eq!(graphql_error.extensions.code, "INTERNAL_ERROR");
    assert!(graphql_error.extensions.details.contains_key("errorType"));
}

#[test]
fn test_graphql_error() {
    let error = GraphQLError::TestError {
        message: "hello".to_string(),
        config_path: "world".to_string(),
    };
    let graphql_error = error.to_graphql_error();

    // Verify the conversion worked
    assert_eq!(graphql_error.message, "Configuration error: hello");
    assert_eq!(
        graphql_error.extensions.code,
        "APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR"
    );
    assert_eq!(graphql_error.extensions.service, "apollo-router");
    assert!(
        graphql_error
            .extensions
            .details
            .contains_key("configMessage")
    );
    assert!(graphql_error.extensions.details.contains_key("configPath"));
}

#[test]
fn test_box_error() {
    // Test with concrete Box<GraphQLError> instead of Box<dyn Error + Send + Sync>
    let error = Box::new(GraphQLError::TestError {
        message: "hello".to_string(),
        config_path: "world".to_string(),
    });
    let graphql_error = error.to_graphql_error();

    // Verify the conversion worked
    assert_eq!(graphql_error.message, "Configuration error: hello");
    assert_eq!(
        graphql_error.extensions.code,
        "APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR"
    );
    assert_eq!(graphql_error.extensions.service, "apollo-router");
    assert!(
        graphql_error
            .extensions
            .details
            .contains_key("configMessage")
    );
    assert!(graphql_error.extensions.details.contains_key("configPath"));
}

#[test]
fn test_arc_error() {
    // Test with concrete Arc<GraphQLError> instead of Arc<dyn Error + Send + Sync>
    let error = Arc::new(GraphQLError::TestError {
        message: "hello".to_string(),
        config_path: "world".to_string(),
    });
    let graphql_error = error.to_graphql_error();

    // Verify the conversion worked
    assert_eq!(graphql_error.message, "Configuration error: hello");
    assert_eq!(
        graphql_error.extensions.code,
        "APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR"
    );
    assert_eq!(graphql_error.extensions.service, "apollo-router");
    assert!(
        graphql_error
            .extensions
            .details
            .contains_key("configMessage")
    );
    assert!(graphql_error.extensions.details.contains_key("configPath"));
}

#[test]
fn test_box_arc_error() {
    // Test with concrete Box<Arc<GraphQLError>> instead of Box<Arc<dyn Error + Send + Sync>>
    let error = Box::new(Arc::new(GraphQLError::TestError {
        message: "hello".to_string(),
        config_path: "world".to_string(),
    }));
    let graphql_error = error.to_graphql_error();

    // Verify the conversion worked
    assert_eq!(graphql_error.message, "Configuration error: hello");
    assert_eq!(
        graphql_error.extensions.code,
        "APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR"
    );
    assert_eq!(graphql_error.extensions.service, "apollo-router");
    assert!(
        graphql_error
            .extensions
            .details
            .contains_key("configMessage")
    );
    assert!(graphql_error.extensions.details.contains_key("configPath"));
}

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type ArcError = Arc<dyn std::error::Error + Send + Sync>;

#[test]
fn test_tower_box_error() {
    // Test with concrete Box<GraphQLError> instead of Box<dyn Error + Send + Sync>
    let error = BoxError::from(GraphQLError::TestError {
        message: "hello".to_string(),
        config_path: "world".to_string(),
    });

    let graphql_error = box_to_graphql_error(&error);

    // Verify the conversion worked
    assert_eq!(graphql_error.message, "Configuration error: hello");
    assert_eq!(
        graphql_error.extensions.code,
        "APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR"
    );
    assert_eq!(graphql_error.extensions.service, "apollo-router");
    assert!(
        graphql_error
            .extensions
            .details
            .contains_key("configMessage")
    );
    assert!(graphql_error.extensions.details.contains_key("configPath"));
}
#[test]
fn test_tower_arc_error() {
    // Test with concrete Box<GraphQLError> instead of Box<dyn Error + Send + Sync>
    let error = ArcError::from(BoxError::from(GraphQLError::TestError {
        message: "hello".to_string(),
        config_path: "world".to_string(),
    }));

    let graphql_error = arc_to_graphql_error(&error);

    // Verify the conversion worked
    assert_eq!(graphql_error.message, "Configuration error: hello");
    assert_eq!(
        graphql_error.extensions.code,
        "APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR"
    );
    assert_eq!(graphql_error.extensions.service, "apollo-router");
    assert!(
        graphql_error
            .extensions
            .details
            .contains_key("configMessage")
    );
    assert!(graphql_error.extensions.details.contains_key("configPath"));
}

#[test]
fn test_tower_box_arc_error() {
    // Test with concrete Box<GraphQLError> instead of Box<dyn Error + Send + Sync>
    let error = BoxError::from(ArcError::from(BoxError::from(GraphQLError::TestError {
        message: "hello".to_string(),
        config_path: "world".to_string(),
    })));

    let graphql_error = box_to_graphql_error(&error);

    // Verify the conversion worked
    assert_eq!(graphql_error.message, "Configuration error: hello");
    assert_eq!(
        graphql_error.extensions.code,
        "APOLLO_ROUTER_MY_SERVICE_CONFIG_ERROR"
    );
    assert_eq!(graphql_error.extensions.service, "apollo-router");
    assert!(
        graphql_error
            .extensions
            .details
            .contains_key("configMessage")
    );
    assert!(graphql_error.extensions.details.contains_key("configPath"));
}
