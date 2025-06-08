use super::*;
use crate::test_utils::TowerTest;
use apollo_compiler::{ExecutableDocument, Schema};
use apollo_federation::query_plan::QueryPlan;
use serde_json::json;
use tower::ServiceExt;

#[derive(Clone)]
struct MockQueryParseService {
    should_fail: bool,
}

impl MockQueryParseService {
    fn new(should_fail: bool) -> Self {
        Self { should_fail }
    }

    fn success() -> Self {
        Self::new(false)
    }

    fn failure() -> Self {
        Self::new(true)
    }
}

impl Service<query_parse::Request> for MockQueryParseService {
    type Response = query_parse::Response;
    type Error = query_parse::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: query_parse::Request) -> Self::Future {
        let should_fail = self.should_fail;
        let extensions = req.extensions;
        let operation_name = req.operation_name;

        Box::pin(async move {
            if should_fail {
                return Err(query_parse::Error::ParsingFailed {
                    message: "Mock parsing error".to_string(),
                });
            }

            // Create a minimal valid schema and document for testing
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
            let doc =
                ExecutableDocument::parse_and_validate(&schema, doc_str, "query.graphql").unwrap();

            Ok(query_parse::Response {
                extensions,
                operation_name,
                query: doc,
            })
        })
    }
}

#[derive(Clone)]
struct MockQueryPlanService {
    should_fail: bool,
}

impl MockQueryPlanService {
    fn new(should_fail: bool) -> Self {
        Self { should_fail }
    }

    fn success() -> Self {
        Self::new(false)
    }

    fn failure() -> Self {
        Self::new(true)
    }
}

impl Service<query_plan::Request> for MockQueryPlanService {
    type Response = query_plan::Response;
    type Error = query_plan::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: query_plan::Request) -> Self::Future {
        let should_fail = self.should_fail;
        let extensions = req.extensions;
        let operation_name = req.operation_name;

        Box::pin(async move {
            if should_fail {
                return Err(query_plan::Error::PlanningFailed {
                    message: "Mock planning error".to_string(),
                });
            }

            // Create a minimal mock query plan
            let query_plan = QueryPlan::default();

            Ok(query_plan::Response {
                extensions,
                operation_name,
                query_plan,
            })
        })
    }
}

#[tokio::test]
async fn test_successful_query_preparation() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = MockQueryParseService::success();
    let plan_service = MockQueryPlanService::success();

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

    let response = service.oneshot(request).await?;

    // Verify response
    assert_eq!(response.operation_name, Some("GetUser".to_string()));
    assert_eq!(response.query_variables.get("id"), Some(&json!("123")));

    // Verify original extensions are preserved
    let context: Option<String> = response.extensions.get();
    assert_eq!(context, Some("test_context".to_string()));

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_with_parse_error() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services with parse service that fails
    let parse_service = MockQueryParseService::failure();
    let plan_service = MockQueryPlanService::success();

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

    // Should fail with parsing error
    assert!(result.is_err());
    match result {
        Err(Error::ParsingFailed { .. }) => {
            // Expected error type
        }
        _ => panic!("Expected ParsingFailed error"),
    }

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_with_plan_error() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services with plan service that fails
    let parse_service = MockQueryParseService::success();
    let plan_service = MockQueryPlanService::failure();

    let parse_service = TowerTest::builder().service(|mut h| async {
        h.next_request()
            .await
            .unwrap()
            .1
            .send_error(query_plan::Error::PlanningFailed {
                message: "failed".to_string(),
            })
    });

    let service = QueryPreparationService::new(parse_service.boxed_clone(), plan_service);

    // Create test request
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "query": "query GetUser { user { id name } }",
            "operationName": "GetUser"
        }),
    };

    let result = service.oneshot(request).await;

    // Should fail with planning error
    assert!(result.is_err());
    match result {
        Err(Error::PlanningFailed { .. }) => {
            // Expected error type
        }
        _ => panic!("Expected PlanningFailed error"),
    }

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_missing_query_field() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = MockQueryParseService::success();
    let plan_service = MockQueryPlanService::success();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request without query field
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "operationName": "SomeOperation"
        }),
    };

    let result = service.oneshot(request).await;

    // Should fail with JSON extraction error
    assert!(result.is_err());
    match result {
        Err(Error::JsonExtraction { field, .. }) => {
            assert_eq!(field, "query");
        }
        _ => panic!("Expected JsonExtraction error"),
    }

    Ok(())
}

#[tokio::test]
async fn test_query_preparation_with_variables() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = MockQueryParseService::success();
    let plan_service = MockQueryPlanService::success();

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

    let response = service.oneshot(request).await?;

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
    let parse_service = MockQueryParseService::success();
    let plan_service = MockQueryPlanService::success();

    let service = QueryPreparationService::new(parse_service, plan_service);

    // Create test request with null variables
    let request = Request {
        extensions: Extensions::default(),
        body: json!({
            "query": "{ user(id: \"123\") { id name } }",
            "variables": null
        }),
    };

    let response = service.oneshot(request).await?;

    // Verify null variables result in empty map
    assert!(response.query_variables.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_extensions_preservation() -> Result<(), Box<dyn std::error::Error>> {
    // Setup mock services
    let parse_service = MockQueryParseService::success();
    let plan_service = MockQueryPlanService::success();

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

    let response = service.oneshot(request).await?;

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
    assert!(result.is_err());

    if let Err(Error::JsonExtraction { field, .. }) = result {
        assert_eq!(field, "query");
    } else {
        panic!("Expected JsonExtraction error");
    }
}

#[tokio::test]
async fn test_extract_graphql_request_invalid_variables() {
    let body = json!({
        "query": "{ user { name } }",
        "variables": "not an object"
    });

    let result = extract_graphql_request(&body);
    assert!(result.is_err());

    match result {
        Err(Error::VariableExtraction { .. }) => {
            // Expected error type
        }
        _ => panic!("Expected VariableExtraction error"),
    }
}
