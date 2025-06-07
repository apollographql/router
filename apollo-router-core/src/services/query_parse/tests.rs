use super::*;
use crate::Extensions;
use serde_json::json;
use tower::{Service, ServiceExt};

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
    
    if let Err(Error::InvalidQuery(msg)) = result {
        assert_eq!(msg, "Query must be a string");
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
    
    if let Err(Error::ParseError(_)) = result {
        // Expected parse error
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
    
    if let Err(Error::ParseError(_)) = result {
        // Expected parse error for empty query
    } else {
        panic!("Expected ParseError for empty query");
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