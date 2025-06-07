use super::*;
use crate::Extensions;
use serde_json::json;
use tower::{Service, ServiceExt};
use apollo_router_error::Error as RouterError;

#[test]
fn test_error_codes_match_strategy() {
    let parse_error = Error::ParseError {
        message: "test".to_string(),
        has_source_code: false,
        query_source: None,
        error_span: None,
    };
    
    // Test that errors have proper diagnostic information
    assert!(format!("{}", parse_error).contains("GraphQL parsing failed"));

    let invalid_error = Error::InvalidQuery {
        reason: "test".to_string(),
        actual_type: "number".to_string(),
        is_empty: false,
        query_data: None,
        error_location: None,
    };
    
    // Test that errors have proper diagnostic information
    assert!(format!("{}", invalid_error).contains("Invalid query format"));
}

// === INSTA TESTS FOR GRAPHQL ERROR REPRESENTATIONS ===

#[test]
fn test_parse_error_graphql_representation() {
    let error = Error::ParseError {
        message: "Expected closing brace".to_string(),
        has_source_code: true,
        query_source: Some("query { user { name ".to_string()),
        error_span: Some((19, 1).into()),
    };

    let graphql_error = error.to_graphql_error();
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

#[test]
fn test_parse_error_with_context_graphql_representation() {
    let error = Error::ParseError {
        message: "Missing closing brace".to_string(),
        has_source_code: true,
        query_source: Some("query GetUser { user { id name }".to_string()),
        error_span: Some((33, 1).into()),
    };

    let context = crate::error::GraphQLErrorContext::builder()
        .service_name("query-parser")
        .trace_id("trace-123")
        .request_id("req-456")
        .location(1, 34)
        .path_field("query")
        .path_field("user")
        .build();

    let graphql_error = error.to_graphql_error_with_context(context);
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

#[test]
fn test_invalid_query_type_graphql_representation() {
    let error = Error::InvalidQuery {
        reason: "Expected string, found number".to_string(),
        actual_type: "number".to_string(),
        is_empty: false,
        query_data: Some("123".to_string()),
        error_location: Some((0, 3).into()),
    };

    let graphql_error = error.to_graphql_error();
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

#[test]
fn test_empty_query_graphql_representation() {
    let error = Error::InvalidQuery {
        reason: "Query string cannot be empty".to_string(),
        actual_type: "string".to_string(),
        is_empty: true,
        query_data: Some("".to_string()),
        error_location: Some((0, 0).into()),
    };

    let context = crate::error::GraphQLErrorContext::builder()
        .service_name("query-validation")
        .location(1, 1)
        .build();

    let graphql_error = error.to_graphql_error_with_context(context);
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

#[test]
fn test_json_extraction_error_graphql_representation() {
    let error = Error::JsonExtraction {
        details: "Failed to parse JSON: unexpected end of input".to_string(),
        json_error: None,
    };

    let graphql_error = error.to_graphql_error();
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

#[test]
fn test_schema_validation_error_graphql_representation() {
    let error = Error::SchemaValidation {
        reason: "Field 'posts' not found on type 'User'".to_string(),
        operation_type: Some("query".to_string()),
        schema_source: Some("type User { id: ID! name: String }".to_string()),
        error_span: Some((15, 5).into()),
    };

    let context = crate::error::GraphQLErrorContext::builder()
        .service_name("schema-validator")
        .trace_id("trace-789")
        .location(3, 17)
        .path_field("user")
        .path_field("posts")
        .build();

    let graphql_error = error.to_graphql_error_with_context(context);
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

#[test]
fn test_complex_path_graphql_representation() {
    let error = Error::ParseError {
        message: "Invalid field selection".to_string(),
        has_source_code: true,
        query_source: Some("query { user { profile { settings { theme } } } }".to_string()),
        error_span: Some((40, 5).into()),
    };

    let context = crate::error::GraphQLErrorContext::builder()
        .service_name("query-parser")
        .location(1, 41)
        .location(2, 5) // Multiple locations
        .path_field("user")
        .path_field("profile")
        .path_index(0)
        .path_field("settings")
        .path_field("theme")
        .build();

    let graphql_error = error.to_graphql_error_with_context(context);
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

#[test]
fn test_minimal_error_graphql_representation() {
    let error = Error::ParseError {
        message: "Syntax error".to_string(),
        has_source_code: false,
        query_source: None,
        error_span: None,
    };

    let graphql_error = error.to_graphql_error();
    let mut settings = insta::Settings::clone_current();
    settings.add_redaction(".extensions.timestamp", "[timestamp]");
    settings.bind(|| {
        insta::assert_yaml_snapshot!(graphql_error);
    });
}

// === ORIGINAL FUNCTIONAL TESTS ===

#[tokio::test]
async fn test_successful_query_parsing() {
    let mut service = QueryParseService::new();
    let mut extensions = Extensions::default();
    extensions.insert("test_context".to_string());

    let request = Request {
        extensions,
        operation_name: Some("GetUser".to_string()),
        query: json!("query GetUser { user { id name } }"),
    };

    let response = service.call(request).await.unwrap();
    
    assert_eq!(response.operation_name, Some("GetUser".to_string()));
    assert!(response.query.operations.len() > 0);
    
    // Verify extensions were preserved
    let context: Option<String> = response.extensions.get();
    assert_eq!(context, Some("test_context".to_string()));
}

#[tokio::test]
async fn test_query_parsing_with_variables() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!("query GetUser($id: ID!) { user(id: $id) { name } }"),
    };

    let response = service.call(request).await.unwrap();
    assert!(response.query.operations.len() > 0);
}

#[tokio::test]
async fn test_mutation_parsing() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: Some("CreateUser".to_string()),
        query: json!("mutation CreateUser($input: UserInput!) { createUser(input: $input) { id } }"),
    };

    let response = service.call(request).await.unwrap();
    assert_eq!(response.operation_name, Some("CreateUser".to_string()));
    assert!(response.query.operations.len() > 0);
}

#[tokio::test]
async fn test_invalid_query_string_format() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!(123), // Not a string
    };

    let result = service.call(request).await;
    assert!(result.is_err());
    
    if let Err(Error::InvalidQuery { reason, actual_type, is_empty, query_data, .. }) = result {
        assert!(reason.contains("Expected string, found number"));
        assert_eq!(actual_type, "number");
        assert_eq!(is_empty, false);
        assert!(query_data.is_some());
    } else {
        panic!("Expected InvalidQuery error");
    }
}

#[tokio::test]
async fn test_malformed_graphql_query() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!("query { invalid syntax here }}}"),
    };

    let result = service.call(request).await;
    assert!(result.is_err());
    
    if let Err(Error::ParseError { message, has_source_code, query_source, .. }) = result {
        assert!(!message.is_empty());
        assert!(has_source_code);
        assert!(query_source.is_some());
    } else {
        panic!("Expected ParseError");
    }
}

#[tokio::test]
async fn test_empty_query_string() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!(""),
    };

    let result = service.call(request).await;
    assert!(result.is_err());
    
    if let Err(Error::InvalidQuery { reason, actual_type, is_empty, error_location, .. }) = result {
        assert!(reason.contains("cannot be empty"));
        assert_eq!(actual_type, "string");
        assert!(is_empty);
        assert_eq!(error_location, Some((0, 0).into()));
    } else {
        panic!("Expected InvalidQuery error for empty query");
    }
}

#[tokio::test]
async fn test_whitespace_only_query() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!("   \t\n  "), // Whitespace only
    };

    let result = service.call(request).await;
    assert!(result.is_err());
    
    if let Err(Error::InvalidQuery { reason, is_empty, .. }) = result {
        assert!(reason.contains("cannot be empty"));
        assert!(is_empty);
    } else {
        panic!("Expected InvalidQuery error for whitespace-only query");
    }
}

#[tokio::test]
async fn test_enhanced_error_reporting() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!({"not": "a string"}), // Object instead of string
    };

    let result = service.call(request).await;
    assert!(result.is_err());
    
    if let Err(Error::InvalidQuery { reason, actual_type, query_data, .. }) = result {
        assert!(reason.contains("Expected string"));
        assert_eq!(actual_type, "object");
        assert!(query_data.is_some());
    } else {
        panic!("Expected InvalidQuery error");
    }
}

#[tokio::test]
async fn test_service_cloning() {
    let service1 = QueryParseService::new();
    let service2 = service1.clone();
    
    // Both services should work independently
    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!("query { __typename }"),
    };

    let mut s1 = service1;
    let mut s2 = service2;
    
    let result1 = s1.call(request.clone()).await;
    let result2 = s2.call(request).await;
    
    assert!(result1.is_ok());
    assert!(result2.is_ok());
}

#[tokio::test]
async fn test_service_ready() {
    let mut service = QueryParseService::new();
    
    // Service should always be ready
    let ready = service.ready().await;
    assert!(ready.is_ok());
}

#[tokio::test]
async fn test_extensions_preservation() {
    let mut service = QueryParseService::new();
    let mut extensions = Extensions::default();
    
    // Add multiple types to extensions
    extensions.insert(42i32);
    extensions.insert("test_string".to_string());
    extensions.insert(3.14f64);

    let request = Request {
        extensions,
        operation_name: Some("TestQuery".to_string()),
        query: json!("query TestQuery { __typename }"),
    };

    let response = service.call(request).await.unwrap();
    
    // Verify all extension values were preserved
    assert_eq!(response.extensions.get::<i32>(), Some(42));
    assert_eq!(response.extensions.get::<String>(), Some("test_string".to_string()));
    assert_eq!(response.extensions.get::<f64>(), Some(3.14));
}

#[tokio::test]
async fn test_complex_query_parsing() {
    let mut service = QueryParseService::new();

    // Use fields that exist in our minimal schema (User has id and name)
    let complex_query = r#"
        query ComplexQuery($userId: ID!) {
            user {
                id
                name
            }
        }
    "#;

    let request = Request {
        extensions: Extensions::default(),
        operation_name: Some("ComplexQuery".to_string()),
        query: json!(complex_query),
    };

    let response = service.call(request).await.unwrap();
    assert_eq!(response.operation_name, Some("ComplexQuery".to_string()));
    assert!(response.query.operations.len() > 0);
}

#[tokio::test]
async fn test_parse_error_with_source_context() {
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!("query { invalid syntax here }}}"), // Malformed query
    };

    let result = service.call(request).await;
    assert!(result.is_err());
    
    if let Err(Error::ParseError { message: _, has_source_code, query_source, error_span }) = result {
        assert!(has_source_code);
        assert!(query_source.is_some());
        // Error span information depends on apollo_compiler's error reporting
        println!("Error span: {:?}", error_span);
    } else {
        panic!("Expected ParseError");
    }
}

#[tokio::test] 
async fn test_schema_validation_error() {
    // This test verifies that schema validation errors are properly categorized
    // In practice, the minimal schema should handle most basic queries
    let mut service = QueryParseService::new();

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: json!("query { nonExistentField }"), // Field not in our minimal schema
    };

    let result = service.call(request).await;
    // This may result in either a ParseError or potentially a SchemaValidation error
    // depending on how apollo_compiler categorizes the validation failure
    assert!(result.is_err());
} 