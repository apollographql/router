use super::*;
use crate::assert_error;
use crate::services::{query_parse, query_plan};
use crate::test_utils::tower_test::{MockService, TowerTest};
use apollo_compiler::{ExecutableDocument, Schema};
use apollo_federation::query_plan::QueryPlan;
use serde_json::json;
use tower::ServiceExt;

// Helper function to create mock services using TowerTest
fn create_successful_parse_service() -> MockService<query_parse::Request, query_parse::Response> {
    TowerTest::builder()
        .service(|mut handle: tower_test::mock::Handle<query_parse::Request, query_parse::Response>| async move {
            handle.allow(1);
            let (req, resp) = handle.next_request().await.expect("should receive parse request");

            // Send successful parse response
            resp.send_response(query_parse::Response {
                extensions: req.extensions,
                operation_name: req.operation_name,
                query: create_test_document(),
            });
        })
}

fn create_failing_parse_service() -> MockService<query_parse::Request, query_parse::Response> {
    TowerTest::builder()
        .service(|mut handle: tower_test::mock::Handle<query_parse::Request, query_parse::Response>| async move {
            handle.allow(1);
            let (_req, resp) = handle.next_request().await.expect("should receive parse request");

            // Send error response
            resp.send_error(query_parse::Error::ParseError {
                message: "Mock parsing error".to_string(),
                errors: vec![],
            });
        })
}

fn create_successful_plan_service() -> MockService<query_plan::Request, query_plan::Response> {
    TowerTest::builder()
        .service(|mut handle: tower_test::mock::Handle<query_plan::Request, query_plan::Response>| async move {
            handle.allow(1);
            let (req, resp) = handle.next_request().await.expect("should receive plan request");

            // Send successful plan response
            resp.send_response(query_plan::Response {
                extensions: req.extensions,
                operation_name: req.operation_name,
                query_plan: QueryPlan::default(),
            });
        })
}

fn create_failing_plan_service() -> MockService<query_plan::Request, query_plan::Response> {
    TowerTest::builder()
        .service(|mut handle: tower_test::mock::Handle<query_plan::Request, query_plan::Response>| async move {
            handle.allow(1);
            let (_req, resp) = handle.next_request().await.expect("should receive plan request");

            // Send error response
            resp.send_error(query_plan::Error::PlanningFailed {
                message: "Mock planning error".to_string(),
            });
        })
}

#[tokio::test]
async fn test_successful_query_preparation() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services using TowerTest
    let parse_service = create_successful_parse_service();
    let plan_service = create_successful_plan_service();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request
    let mut extensions = Extensions::default();
    extensions.insert("test_context".to_string());

    let request = Request {
        extensions: extensions.clone(),
        body: json!({
            "query": "query GetUser($id: ID!) { user(id: $id) { id name } }",
            "operationName": "GetUser",
            "variables": { "id": "123" }
        }),
    };

    let response = service
        .oneshot(request)
        .await
        .map_err(|e| format!("Service call failed: {}", e))?;

    // Verify response
    assert_eq!(response.operation_name, Some("GetUser".to_string()));
    assert_eq!(response.query_variables.get("id"), Some(&json!("123")));

    // Verify original extensions are preserved
    let test_value: Option<String> = response.extensions.get();
    assert_eq!(test_value, Some("test_context".to_string()));

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_with_parse_error() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services with parse service that fails
    let parse_service = create_failing_parse_service();
    let plan_service = create_successful_plan_service();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "query": "invalid graphql query {{{",
            "operationName": "InvalidQuery"
        }),
    };

    let result = service.oneshot(request).await;

    // Should fail with parsing error - now passes through the original query_parse::Error
    assert_error!(
        result,
        query_parse::Error,
        query_parse::Error::ParseError { .. }
    );

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_with_plan_error() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services with plan service that fails
    let parse_service = create_successful_parse_service();
    let plan_service = create_failing_plan_service();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "query": "query GetUser { user { id name } }",
            "operationName": "GetUser"
        }),
    };

    let result = service.oneshot(request).await;

    // Should fail with planning error - now passes through the original query_plan::Error
    assert_error!(
        result,
        query_plan::Error,
        query_plan::Error::PlanningFailed { .. }
    );

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_missing_query_field() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = create_successful_parse_service();
    let plan_service = create_successful_plan_service();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request without query field
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "operationName": "SomeOperation"
        }),
    };

    let result = service.oneshot(request).await;

    // Should fail with JSON extraction error for missing query field
    assert_error!(result, Error, Error::JsonExtraction { field, .. } => {
        assert_eq!(field, "query");
    });

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_with_variables() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = create_successful_parse_service();
    let plan_service = create_successful_plan_service();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request with complex variables
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "query": "query GetUser($id: ID!, $includeProfile: Boolean) { user(id: $id) { id name } }",
            "variables": {
                "id": "user123",
                "includeProfile": true
            }
        }),
    };

    let response = service
        .oneshot(request)
        .await
        .map_err(|e| format!("Service call failed: {}", e))?;

    // Verify variables are correctly extracted
    assert_eq!(response.query_variables.get("id"), Some(&json!("user123")));
    assert_eq!(
        response.query_variables.get("includeProfile"),
        Some(&json!(true))
    );

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_null_variables() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = create_successful_parse_service();
    let plan_service = create_successful_plan_service();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request with null variables
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "query": "{ user(id: \"123\") { id name } }",
            "variables": null
        }),
    };

    let response = service
        .oneshot(request)
        .await
        .map_err(|e| format!("Service call failed: {}", e))?;

    // Verify null variables result in empty map
    assert!(response.query_variables.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_extensions_preservation() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = create_successful_parse_service();
    let plan_service = create_successful_plan_service();
    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request with multiple extension values
    let mut extensions = Extensions::default();
    extensions.insert("upstream_value".to_string());
    extensions.insert(42i32);

    let request = Request {
        extensions,
        body: json!({
            "query": "{ user(id: \"123\") { id name } }"
        }),
    };

    let response = service
        .oneshot(request)
        .await
        .map_err(|e| format!("Service call failed: {}", e))?;

    // Verify original extensions are preserved (not extended layers)
    let upstream_string: Option<String> = response.extensions.get();
    assert_eq!(upstream_string, Some("upstream_value".to_string()));

    let upstream_int: Option<i32> = response.extensions.get();
    assert_eq!(upstream_int, Some(42));

    Ok(())
}

#[tokio::test]
async fn test_extract_graphql_request_valid() {
    let body = json!({
        "query": "{ user { name } }",
        "operationName": "GetUser",
        "variables": { "id": "123" }
    });

    let (query, operation_name, variables) = extract_graphql_request(&body).unwrap();

    assert_eq!(query, "{ user { name } }");
    assert_eq!(operation_name, Some("GetUser".to_string()));
    assert_eq!(variables.get("id"), Some(&json!("123")));
}

#[tokio::test]
async fn test_extract_graphql_request_minimal() {
    let body = json!({
        "query": "{ user { name } }"
    });

    let (query, operation_name, variables) = extract_graphql_request(&body).unwrap();

    assert_eq!(query, "{ user { name } }");
    assert_eq!(operation_name, None);
    assert!(variables.is_empty());
}

#[tokio::test]
async fn test_extract_graphql_request_missing_query() {
    let body = json!({
        "operationName": "GetUser"
    });

    let result = extract_graphql_request(&body);

    // Should fail with JSON extraction error for missing query field
    assert_error!(result, Error::JsonExtraction { field, .. } => {
        assert_eq!(field, "query");
    });
}

#[tokio::test]
async fn test_extract_graphql_request_invalid_variables() {
    let body = json!({
        "query": "{ user { name } }",
        "variables": "not an object"
    });

    let result = extract_graphql_request(&body);

    // Should fail with variable extraction error
    assert_error!(result, Error::VariableExtraction { .. });
}

fn create_test_document() -> apollo_compiler::validation::Valid<ExecutableDocument> {
    let schema_str = r#"
        type Query {
            user(id: ID!): User
        }
        type User {
            id: ID!
            name: String!
        }
    "#;
    let schema = Schema::parse_and_validate(schema_str, "test.graphql").unwrap();
    let doc_str = "query { user(id: \"123\") { id name } }";
    ExecutableDocument::parse_and_validate(&schema, doc_str, "query.graphql").unwrap()
}
