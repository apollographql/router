use crate::assert_error;
use crate::Extensions;
use crate::services::query_parse::{Error, ParseErrorDetail, QueryParseService, Request};
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
    assert_error!(result, Error::ParsingFailed { message } => {
        assert!(message.contains("nonExistentField") || message.contains("does not have a field"));
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
    assert_error!(invalid_result, Error::ParsingFailed { message } => {
        assert!(message.contains("unknownField") || message.contains("does not have a field"));
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

    // This syntax error consistently returns MultipleParsingErrors
    assert_error!(result, Error::MultipleParsingErrors { errors, .. } => {
        // Should contain syntax errors
        let has_syntax_errors = errors.iter().any(|e| matches!(e, ParseErrorDetail::SyntaxError { .. }));
        assert!(has_syntax_errors, "Should contain syntax errors");
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

    assert_error!(result, Error::ParsingFailed { message } => {
        assert!(message.contains("Unknown field") || message.contains("unknownField"));
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

    assert_error!(result, Error::MultipleParsingErrors { count, errors } => {
        assert!(*count > 1);
        assert!(errors.len() > 1);
        // Should contain multiple validation errors
        let has_validation_errors = errors.iter().any(|e| matches!(e, ParseErrorDetail::ValidationError { .. }));
        assert!(has_validation_errors, "Should contain validation errors for unknown fields");
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

    assert_error!(result, Error::ParsingFailed { message } => {
        assert!(message.contains("syntax") || message.contains("expected"));
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

    // This validation error consistently returns MultipleParsingErrors
    assert_error!(result, Error::MultipleParsingErrors { errors, .. } => {
        // Should contain validation errors for unknown field
        let has_unknown_field_error = errors.iter().any(|e| {
            if let ParseErrorDetail::ValidationError { message, .. } = e {
                message.contains("unknownField")
            } else {
                false
            }
        });
        assert!(has_unknown_field_error, "Should contain validation error for unknownField");
    });
}

#[tokio::test]
async fn test_error_categorization() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);

    // Test different types of errors to ensure proper categorization
    let test_cases: Vec<(&str, fn(&[ParseErrorDetail]) -> bool)> = vec![
        (
            r#"query { user( }"#, // Syntax error
            |errors: &[ParseErrorDetail]| {
                errors
                    .iter()
                    .any(|e| matches!(e, ParseErrorDetail::SyntaxError { .. }))
            },
        ),
        (
            r#"query { user(id: "123") { unknownField } }"#, // Unknown field
            |errors: &[ParseErrorDetail]| {
                errors.iter().any(|e| {
                    matches!(
                        e,
                        ParseErrorDetail::UnknownField { .. }
                            | ParseErrorDetail::ValidationError { .. }
                    )
                })
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
        
        // Verify we get the correct error categorization for each query type
        assert_error!(result, Error::MultipleParsingErrors { errors, .. } => {
            assert!(error_check(&errors), "Error categorization failed for query: {}", query);
        });
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
async fn test_error_location_information() {
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

    // Verify we get multiple validation errors for the unknown fields
    assert_error!(result, Error::MultipleParsingErrors { count, errors } => {
        assert!(*count >= 4, "Should have at least 4 validation errors");
        assert!(errors.len() >= 4, "Should have error details for each invalid field");
        // Should all be validation errors for unknown fields
        let all_validation_errors = errors.iter().all(|e| matches!(e, ParseErrorDetail::ValidationError { .. }));
        assert!(all_validation_errors, "All errors should be validation errors");
    });
}

#[test]
fn test_extract_quoted_text() {
    use crate::services::query_parse::extract_quoted_text;

    let text = "Unknown field 'fieldName' on type 'TypeName'";
    assert_eq!(extract_quoted_text(text, 0), Some("fieldName".to_string()));
    assert_eq!(extract_quoted_text(text, 1), Some("TypeName".to_string()));
    assert_eq!(extract_quoted_text(text, 2), None);

    let no_quotes = "No quotes here";
    assert_eq!(extract_quoted_text(no_quotes, 0), None);

    let single_quote = "Only 'one' quote";
    assert_eq!(
        extract_quoted_text(single_quote, 0),
        Some("one".to_string())
    );
    assert_eq!(extract_quoted_text(single_quote, 1), None);
}
