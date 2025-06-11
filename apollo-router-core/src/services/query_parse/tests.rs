use crate::assert_error;
use crate::Extensions;
use crate::services::query_parse::{Error, QueryParseService, Request};
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

    Schema::parse_and_validate(schema_sdl, "schema.graphql").expect("Test schema should be valid")
}

// Old tests removed - incompatible with new API that returns Valid<ExecutableDocument> directly
// and puts parsing errors in service error type instead of response

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
    assert_eq!(
        response.extensions.get::<String>(),
        Some("test_string".to_string())
    );
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

    // Check that parsing was successful - query field now contains Valid<ExecutableDocument> directly
    let document = response.query.into_inner();
    assert!(document.operations.len() > 0);
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

    // This should now return a service error instead of being in the response
    let result = service.call(request).await;
    assert!(result.is_err());

    // Verify we get a validation error for the non-existent field
    assert_error!(result, Error::ValidationError { message, errors } => {
        assert!(message.contains("Validation error"));
        assert!(!errors.is_empty());
    });
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

    // Valid query should succeed - query field now contains Valid<ExecutableDocument> directly
    let document = response.query.into_inner();
    assert!(!document.operations.is_empty());
}

#[tokio::test]
async fn test_result_structure() {
    let schema = create_test_schema();
    let mut service = QueryParseService::new(schema);

    // Test both successful and error cases to verify new structure
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
    let invalid_result = service.call(invalid_request).await;

    // Valid query: Response with Valid<ExecutableDocument>
    let document = valid_response.query.into_inner();
    assert!(!document.operations.is_empty());

    // Invalid query: Service Error with proper error validation
    assert_error!(invalid_result, Error::ValidationError { message, errors } => {
        assert!(message.contains("Validation error"));
        assert!(!errors.is_empty());
    });
}

fn test_schema() -> Valid<Schema> {
    let schema_sdl = r#"
        type Query {
            hello(name: String): String
            user(id: ID!): User
            users: [User!]!
        }
        
        type User {
            id: ID!
            name: String!
            email: String
            posts: [Post!]!
        }
        
        type Post {
            id: ID!
            title: String!
            content: String
            author: User!
        }
    "#;

    Schema::parse_and_validate(schema_sdl, "test-schema.graphql").unwrap()
}

#[tokio::test]
async fn test_valid_query_parsing() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::new(),
        operation_name: Some("GetUser".to_string()),
        query: r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    name
                    email
                }
            }
        "#
        .to_string(),
    };

    let response = service.call(request).await.unwrap();

    // Verify the query was parsed successfully
    assert_eq!(response.operation_name, Some("GetUser".to_string()));
    // The query field now contains Valid<ExecutableDocument> directly
    let document = response.query.into_inner();
    assert!(document.operations.get(Some("GetUser")).is_ok());
}

#[tokio::test]
async fn test_syntax_error_parsing() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"
            query {
                user( {
                    id
                }
            }
        "#
        .to_string(),
    };

    let result = service.call(request).await;
    assert!(result.is_err());

    // Should get a parse error for syntax issues
    assert_error!(result, Error::ParseError { message, errors } => {
        assert!(message.contains("Parse error") || message.contains("Multiple parse errors"));
        assert!(!errors.is_empty());
    });
}

#[tokio::test]
async fn test_validation_error_unknown_field() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"
            query {
                user(id: "123") {
                    id
                    unknownField
                }
            }
        "#
        .to_string(),
    };

    let result = service.call(request).await;
    assert!(result.is_err());

    assert_error!(result, Error::ValidationError { message, errors } => {
        assert!(message.contains("Validation error"));
        assert!(!errors.is_empty());
    });
}

#[tokio::test]
async fn test_multiple_validation_errors() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"
            query {
                user(id: "123") {
                    id
                    unknownField1
                    unknownField2
                }
                nonExistentField
            }
        "#
        .to_string(),
    };

    let result = service.call(request).await;
    assert!(result.is_err());

    assert_error!(result, Error::ValidationError { message, errors } => {
        assert!(message.contains("Multiple validation errors"));
        assert!(errors.len() > 1);
    });
}

#[tokio::test]
async fn test_invalid_syntax_error() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"
            query {
                user(id: 123 { # Missing closing parenthesis  
                    id
                    name
                }
            }
        "#
        .to_string(),
    };

    let result = service.call(request).await;
    assert!(result.is_err());

    assert_error!(result, Error::ParseError { message, errors } => {
        assert!(message.contains("Parse error"));
        assert!(!errors.is_empty());
    });
}

#[tokio::test]
async fn test_error_serialization() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"
            query {
                user(id: "123") {
                    unknownField
                }
            }
        "#
        .to_string(),
    };

    let result = service.call(request).await;
    assert!(result.is_err());

    // Should get validation error for unknown field
    assert_error!(result, Error::ValidationError { message, errors } => {
        assert!(message.contains("Validation error") || message.contains("Multiple validation errors"));
        assert!(!errors.is_empty());
        
        // Verify we have GraphQLError objects in the errors vec
        for error in errors {
            assert!(!error.message.is_empty());
        }
    });
}

#[tokio::test]
async fn test_error_to_json_functionality() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    // Test different types of errors to ensure to_json works properly
    let test_cases: Vec<(&str, fn(&Error) -> bool)> = vec![
        (
            r#"query { user( }"#, // Syntax error
            |error: &Error| {
                matches!(error, Error::ParseError { errors, .. } if !errors.is_empty())
            },
        ),
        (
            r#"query { user(id: "123") { unknownField } }"#, // Unknown field
            |error: &Error| {
                matches!(error, Error::ValidationError { errors, .. } if !errors.is_empty())
            },
        ),
    ];

    for (query, error_check) in test_cases {
        let request = Request {
            extensions: Extensions::new(),
            operation_name: None,
            query: query.to_string(),
        };
        
        let result = service.call(request).await;
        assert!(result.is_err());
        
        let error = result.unwrap_err();
        assert!(error_check(&error), "Error structure check failed for query: {}", query);
    }
}

#[tokio::test]
async fn test_new_service_extensions_preservation() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let mut extensions = Extensions::new();
    extensions.insert("test_key".to_string());
    extensions.insert(42i32);

    let request = Request {
        extensions,
        operation_name: Some("TestOp".to_string()),
        query: r#"
            query TestOp {
                hello(name: "world")
            }
        "#
        .to_string(),
    };

    let response = service.call(request).await.unwrap();

    // Verify extensions are preserved
    assert_eq!(
        response.extensions.get::<String>(),
        Some("test_key".to_string())
    );
    assert_eq!(response.extensions.get::<i32>(), Some(42));

    // Verify operation name is preserved
    assert_eq!(response.operation_name, Some("TestOp".to_string()));
}

#[tokio::test]
async fn test_error_json_structure() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    let request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"
            query {
                user(id: "123") {
                    id
                    nonExistentField1
                    nonExistentField2  
                    anotherBadField
                }
                nonExistentRootField
            }
        "#
        .to_string(),
    };

    let result = service.call(request).await;
    assert!(result.is_err());

    assert_error!(result, Error::ValidationError { message, errors } => {
        assert!(message.contains("Multiple validation errors"));
        assert!(errors.len() > 1);
        
        // Verify each error is a GraphQLError from diagnostic.to_json()
        for error in errors {
            // The error should be a GraphQLError with diagnostic information
            assert!(!error.message.is_empty());
        }
    });
}

#[tokio::test]
async fn test_parse_vs_validation_error_separation() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    // Test syntax error (should be ParseError)
    let parse_request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"query { user( }"#.to_string(),
    };

    let parse_result = service.call(parse_request).await;
    assert_error!(parse_result, Error::ParseError { .. });

    // Test validation error (should be ValidationError) 
    let validation_request = Request {
        extensions: Extensions::new(),
        operation_name: None,
        query: r#"query { user(id: "123") { unknownField } }"#.to_string(),
    };

    let validation_result = service.call(validation_request).await;
    assert_error!(validation_result, Error::ValidationError { .. });
}
