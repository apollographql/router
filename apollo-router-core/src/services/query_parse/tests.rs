use super::*;
use crate::Extensions;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use tower::{Service, ServiceExt};

fn create_test_schema() -> Valid<Schema> {
    let schema_sdl = r#"
        type Query {
            user: User
        }
        
        type User {
            id: ID!
            name: String
        }
        
        type Mutation {
            createUser(input: UserInput!): User
        }
        
        input UserInput {
            name: String!
        }
    "#;
    
    Schema::parse_and_validate(schema_sdl, "schema.graphql")
        .expect("Test schema should be valid")
}

#[tokio::test]
async fn test_successful_query_parsing() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);
    let mut extensions = Extensions::default();
    extensions.insert("test_context".to_string());

    let request = Request {
        extensions,
        operation_name: Some("GetUser".to_string()),
        query: "query GetUser { user { id name } }".to_string(),
    };

    let response = service.call(request).await.unwrap();
    
    assert_eq!(response.operation_name, Some("GetUser".to_string()));
    
    // Check that parsing was successful (Ok case)
    assert!(response.query.is_ok());
    if let Ok(valid_doc) = &response.query {
        assert!(valid_doc.operations.len() > 0);
    }
    
    // Verify extensions were preserved
    let context: Option<String> = response.extensions.get();
    assert_eq!(context, Some("test_context".to_string()));
}

#[tokio::test]
async fn test_query_parsing_with_variables() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "query { user { name } }".to_string(),
    };

    let response = service.call(request).await.unwrap();
    
    // Check that parsing was successful
    assert!(response.query.is_ok());
    if let Ok(valid_doc) = &response.query {
        assert!(valid_doc.operations.len() > 0);
    }
}

#[tokio::test]
async fn test_mutation_parsing() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::default(),
        operation_name: Some("CreateUser".to_string()),
        query: "mutation CreateUser($input: UserInput!) { createUser(input: $input) { id } }".to_string(),
    };

    let response = service.call(request).await.unwrap();
    assert_eq!(response.operation_name, Some("CreateUser".to_string()));
    
    // Check that parsing was successful
    assert!(response.query.is_ok());
    if let Ok(valid_doc) = &response.query {
        assert!(valid_doc.operations.len() > 0);
    }
}

#[tokio::test]
async fn test_empty_query_string() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "".to_string(),
    };

    let response = service.call(request).await.unwrap();
    
    // Empty queries should return Err(WithErrors) from apollo_compiler
    assert!(response.query.is_err());
    if let Err(with_errors) = &response.query {
        assert!(!with_errors.errors.is_empty());
    }
}

#[tokio::test]
async fn test_whitespace_only_query() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "   \t\n  ".to_string(), // Whitespace only
    };

    let response = service.call(request).await.unwrap();
    
    // Whitespace-only queries should return Err(WithErrors) from apollo_compiler  
    assert!(response.query.is_err());
    if let Err(with_errors) = &response.query {
        assert!(!with_errors.errors.is_empty());
    }
}

#[tokio::test]
async fn test_malformed_graphql_query() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "query { invalid syntax here }}}".to_string(),
    };

    let response = service.call(request).await.unwrap();
    
    // Malformed queries should return Err(WithErrors)
    assert!(response.query.is_err());
    if let Err(with_errors) = &response.query {
        assert!(!with_errors.errors.is_empty());
        // But we should still get a partial document
        assert!(!with_errors.partial.operations.is_empty());
    }
    assert_eq!(response.operation_name, None);
}

#[tokio::test]
async fn test_service_cloning() {
    let schema = create_test_schema();
    let service1 = QueryParseService::new(schema.clone());
    let service2 = QueryParseService::new(schema);
    
    // Both services should work independently
    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "query { __typename }".to_string(),
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
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);
    
    // Service should always be ready
    let ready = service.ready().await;
    assert!(ready.is_ok());
}

#[tokio::test]
async fn test_extensions_preservation() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);
    let mut extensions = Extensions::default();
    
    // Add multiple types to extensions
    extensions.insert(42i32);
    extensions.insert("test_string".to_string());
    extensions.insert(3.14f64);

    let request = Request {
        extensions,
        operation_name: Some("TestQuery".to_string()),
        query: "query TestQuery { __typename }".to_string(),
    };

    let response = service.call(request).await.unwrap();
    
    // Verify all extension values were preserved
    assert_eq!(response.extensions.get::<i32>(), Some(42));
    assert_eq!(response.extensions.get::<String>(), Some("test_string".to_string()));
    assert_eq!(response.extensions.get::<f64>(), Some(3.14));
}

#[tokio::test]
async fn test_complex_query_parsing() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let complex_query = r#"
        query ComplexQuery {
            user {
                id
                name
            }
        }
    "#;

    let request = Request {
        extensions: Extensions::default(),
        operation_name: Some("ComplexQuery".to_string()),
        query: complex_query.to_string(),
    };

    let response = service.call(request).await.unwrap();
    assert_eq!(response.operation_name, Some("ComplexQuery".to_string()));
    
    // Check that parsing was successful
    assert!(response.query.is_ok());
    if let Ok(valid_doc) = &response.query {
        assert!(valid_doc.operations.len() > 0);
    }
}

#[tokio::test]
async fn test_invalid_field_query() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "query { nonExistentField }".to_string(), // Field not in our schema
    };

    let response = service.call(request).await.unwrap();
    
    // This should result in validation errors in the Err(WithErrors) response
    assert!(response.query.is_err());
    if let Err(with_errors) = &response.query {
        assert!(!with_errors.errors.is_empty());
        // But we should still get a partial document
        assert!(!with_errors.partial.operations.is_empty());
    }
}

#[tokio::test]
async fn test_successful_parsing_no_errors() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "query { user { id } }".to_string(),
    };

    let response = service.call(request).await.unwrap();
    
    // Valid query should have Ok result
    assert!(response.query.is_ok());
    if let Ok(valid_doc) = &response.query {
        assert!(!valid_doc.operations.is_empty());
    }
}

#[tokio::test]
async fn test_result_structure() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    // Test both successful and error cases to verify Result structure
    let valid_request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "query { user { id } }".to_string(),
    };

    let invalid_request = Request {
        extensions: Extensions::default(),
        operation_name: None,
        query: "query { unknownField }".to_string(),
    };

    let valid_response = service.call(valid_request).await.unwrap();
    let invalid_response = service.call(invalid_request).await.unwrap();

    // Valid query: Ok(Valid<ExecutableDocument>)
    assert!(valid_response.query.is_ok());
    if let Ok(valid_doc) = &valid_response.query {
        assert!(!valid_doc.operations.is_empty());
    }

    // Invalid query: Err(WithErrors<ExecutableDocument>)
    assert!(invalid_response.query.is_err());
    if let Err(with_errors) = &invalid_response.query {
        assert!(!with_errors.errors.is_empty());
        assert!(!with_errors.partial.operations.is_empty());
    }
} 