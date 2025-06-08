use super::*;
use crate::Extensions;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use tower::{Service, ServiceExt};
use crate::services::query_parse::{Error, ParseErrorDetail, QueryParseService, Request, Response};

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

    // Invalid query: Service Error
    assert!(invalid_result.is_err());
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
        "#.to_string(),
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
        "#.to_string(),
    };
    
    let result = service.call(request).await;
    assert!(result.is_err());
    
    let error = result.unwrap_err();
    match error {
        Error::ParsingFailed { message } => {
            assert!(message.contains("syntax") || message.contains("expected"));
        }
        Error::MultipleParsingErrors { count, errors } => {
            assert!(count > 0);
            assert!(!errors.is_empty());
            // Should contain syntax error variants
            assert!(errors.iter().any(|e| matches!(e, ParseErrorDetail::SyntaxError { .. })));
        }
        _ => panic!("Expected parsing error, got: {:?}", error),
    }
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
        "#.to_string(),
    };
    
    let result = service.call(request).await;
    assert!(result.is_err());
    
    let error = result.unwrap_err();
    match error {
        Error::ParsingFailed { message } => {
            assert!(message.contains("Unknown field") || message.contains("unknownField"));
        }
        Error::MultipleParsingErrors { errors, .. } => {
            assert!(errors.iter().any(|e| matches!(e, ParseErrorDetail::UnknownField { .. })));
        }
        _ => panic!("Expected parsing/validation error, got: {:?}", error),
    }
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
        "#.to_string(),
    };
    
    let result = service.call(request).await;
    assert!(result.is_err());
    
    let error = result.unwrap_err();
    match error {
        Error::MultipleParsingErrors { count, errors } => {
            assert!(count > 1);
            assert!(errors.len() > 1);
            
            // Should contain multiple error types
            let has_unknown_field = errors.iter().any(|e| matches!(e, ParseErrorDetail::UnknownField { .. }));
            let has_validation_error = errors.iter().any(|e| matches!(e, ParseErrorDetail::ValidationError { .. }));
            
            assert!(has_unknown_field || has_validation_error);
        }
        Error::ParsingFailed { .. } => {
            // Single error case is also acceptable
        }
        _ => panic!("Expected parsing error, got: {:?}", error),
    }
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
        "#.to_string(),
    };
    
    let result = service.call(request).await;
    assert!(result.is_err());
    
    let error = result.unwrap_err();
    match error {
        Error::ParsingFailed { message } => {
            // Syntax error should be caught
            assert!(!message.is_empty());
        }
        Error::MultipleParsingErrors { errors, .. } => {
            assert!(!errors.is_empty());
            // Should contain syntax or validation error
            let has_syntax_or_validation = errors.iter().any(|e| {
                matches!(e, ParseErrorDetail::SyntaxError { .. } | ParseErrorDetail::ValidationError { .. })
            });
            assert!(has_syntax_or_validation);
        }
        _ => panic!("Expected parsing error, got: {:?}", error),
    }
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
        "#.to_string(),
    };
    
    let result = service.call(request).await;
    assert!(result.is_err());
    
    let error = result.unwrap_err();
    
    // Test that errors can be serialized for GraphQL extensions
    let json_result = serde_json::to_string(&error);
    assert!(json_result.is_ok());
    
    // Test specific error detail serialization
    match error {
        Error::ParsingFailed { .. } => {
            // Single error case
        }
        Error::MultipleParsingErrors { errors, .. } => {
            for error_detail in errors {
                let detail_json = serde_json::to_string(&error_detail);
                assert!(detail_json.is_ok());
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn test_error_categorization() {
    let schema = test_schema();
    let mut service = QueryParseService::new(schema);
    
    // Test different types of errors to ensure proper categorization
    let test_cases: Vec<(&str, fn(&[ParseErrorDetail]) -> bool)> = vec![
        (
            r#"query { user( }"#,  // Syntax error
            |errors: &[ParseErrorDetail]| {
                errors.iter().any(|e| matches!(e, ParseErrorDetail::SyntaxError { .. }))
            }
        ),
        (
            r#"query { user(id: "123") { unknownField } }"#,  // Unknown field
            |errors: &[ParseErrorDetail]| {
                errors.iter().any(|e| matches!(e, ParseErrorDetail::UnknownField { .. } | ParseErrorDetail::ValidationError { .. }))
            }
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
        match error {
            Error::ParsingFailed { .. } => {
                // Single error case - can't test categorization as easily
            }
            Error::MultipleParsingErrors { errors, .. } => {
                assert!(error_check(&errors), "Error categorization failed for query: {}", query);
            }
            _ => panic!("Unexpected error type for query: {}", query),
        }
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
        "#.to_string(),
    };
    
    let response = service.call(request).await.unwrap();
    
    // Verify extensions are preserved
    assert_eq!(response.extensions.get::<String>(), Some("test_key".to_string()));
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
        "#.to_string(),
    };
    
    let result = service.call(request).await;
    assert!(result.is_err());
    
    let error = result.unwrap_err();
    
    // Use insta snapshot to verify error structure and location information
    insta::assert_yaml_snapshot!(error, @r###"
    MultipleParsingErrors:
      count: 4
      errors:
        - ValidationError:
            message: "Error: type `User` does not have a field `nonExistentField1`\n   ╭─[ query.graphql:5:21 ]\n   │\n 5 │                     nonExistentField1\n   │                     ────────┬────────  \n   │                             ╰────────── field `nonExistentField1` selected here\n   │\n   ├─[ test-schema.graphql:8:14 ]\n   │\n 8 │         type User {\n   │              ──┬─  \n   │                ╰─── type `User` defined here\n   │ \n   │ Note: path to the field: `query → user → nonExistentField1`\n───╯\n"
            line: 5
            column: 21
        - ValidationError:
            message: "Error: type `User` does not have a field `nonExistentField2`\n   ╭─[ query.graphql:6:21 ]\n   │\n 6 │                     nonExistentField2\n   │                     ────────┬────────  \n   │                             ╰────────── field `nonExistentField2` selected here\n   │\n   ├─[ test-schema.graphql:8:14 ]\n   │\n 8 │         type User {\n   │              ──┬─  \n   │                ╰─── type `User` defined here\n   │ \n   │ Note: path to the field: `query → user → nonExistentField2`\n───╯\n"
            line: 6
            column: 21
        - ValidationError:
            message: "Error: type `User` does not have a field `anotherBadField`\n   ╭─[ query.graphql:7:21 ]\n   │\n 7 │                     anotherBadField\n   │                     ───────┬───────  \n   │                            ╰───────── field `anotherBadField` selected here\n   │\n   ├─[ test-schema.graphql:8:14 ]\n   │\n 8 │         type User {\n   │              ──┬─  \n   │                ╰─── type `User` defined here\n   │ \n   │ Note: path to the field: `query → user → anotherBadField`\n───╯\n"
            line: 7
            column: 21
        - ValidationError:
            message: "Error: type `Query` does not have a field `nonExistentRootField`\n   ╭─[ query.graphql:9:17 ]\n   │\n 9 │                 nonExistentRootField\n   │                 ──────────┬─────────  \n   │                           ╰─────────── field `nonExistentRootField` selected here\n   │\n   ├─[ test-schema.graphql:2:14 ]\n   │\n 2 │         type Query {\n   │              ──┬──  \n   │                ╰──── type `Query` defined here\n   │ \n   │ Note: path to the field: `query → nonExistentRootField`\n───╯\n"
            line: 9
            column: 17
    "###);
}

#[cfg(test)]
mod tests {
    use super::*;
    
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
        assert_eq!(extract_quoted_text(single_quote, 0), Some("one".to_string()));
        assert_eq!(extract_quoted_text(single_quote, 1), None);
    }
} 
