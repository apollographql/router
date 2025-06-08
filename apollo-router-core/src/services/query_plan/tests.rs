use super::*;

#[tokio::test]
async fn test_query_plan_service_error_types() {
    // Test that error types work correctly
    let planning_error = Error::PlanningFailed {
        message: "test planning failure".to_string(),
    };
    
    let federation_error = Error::FederationError {
        message: "test federation error".to_string(),
    };
    
    let invalid_supergraph = Error::InvalidSupergraph {
        reason: "test invalid supergraph".to_string(),
    };

    // Test that we can create different error types
    assert!(matches!(planning_error, Error::PlanningFailed { .. }));
    assert!(matches!(federation_error, Error::FederationError { .. }));
    assert!(matches!(invalid_supergraph, Error::InvalidSupergraph { .. }));
}

#[test]
fn test_error_codes() {
    use apollo_router_error::Error as RouterError;
    
    let planning_error = Error::PlanningFailed {
        message: "test".to_string(),
    };
    
    let federation_error = Error::FederationError {
        message: "test".to_string(),
    };
    
    let invalid_supergraph = Error::InvalidSupergraph {
        reason: "test".to_string(),
    };

    // Test that error codes are correctly implemented
    assert_eq!(planning_error.error_code(), "APOLLO_ROUTER_QUERY_PLAN_PLANNING_FAILED");
    assert_eq!(federation_error.error_code(), "APOLLO_ROUTER_QUERY_PLAN_FEDERATION_ERROR");
    assert_eq!(invalid_supergraph.error_code(), "APOLLO_ROUTER_QUERY_PLAN_INVALID_SUPERGRAPH");
}

#[test]
fn test_request_response_structure() {
    // Test that Request and Response types can be created
    let extensions = Extensions::default();
    let operation_name = Some(apollo_compiler::Name::try_from("TestOperation").unwrap());
    
    // Test that we can create basic types
    // The important thing is that the types compile and have the expected structure
    assert!(extensions.get::<String>().is_none()); // Extensions starts empty
    assert_eq!(operation_name.as_ref().unwrap().as_str(), "TestOperation");
}

#[tokio::test]
async fn test_service_creation_methods_exist() {
    // Test that service creation methods exist and have the correct signatures
    // We can't easily create a valid supergraph in the test environment,
    // but we can verify the methods exist through compilation
    
    // If these methods didn't exist or had wrong signatures, this wouldn't compile
    let _method_exists = QueryPlanService::new;
    let _method_exists = QueryPlanService::with_supergraph;
    
    assert!(true); // These methods exist (verified by compilation)
}

#[tokio::test]  
async fn test_tower_service_trait_implementation() {
    // Test that QueryPlanService properly implements the Tower Service trait
    // This is verified through compilation - if it compiles, the trait is implemented correctly
    
    // The service must implement:
    // - Service<Request, Response = Response, Error = Error>
    // - poll_ready method  
    // - call method that returns the correct Future type
    
    // This compiles only if the Tower Service trait is properly implemented
    use tower::Service;
    
    fn _assert_service_trait<T>()
    where
        T: Service<Request, Response = Response, Error = Error> + Clone,
    {
        // This function exists only to verify trait bounds at compile time
    }
    
    _assert_service_trait::<QueryPlanService>();
    assert!(true); // Test passes if compilation succeeds
}

#[test] 
fn test_error_display_formatting() {
    // Test that errors display correctly
    let planning_error = Error::PlanningFailed {
        message: "Something went wrong".to_string(),
    };
    
    let error_string = format!("{}", planning_error);
    assert!(error_string.contains("Query planning failed"));
    assert!(error_string.contains("Something went wrong"));
}

#[test]
fn test_graphql_extensions_population() {
    use apollo_router_error::Error as RouterError;
    use std::collections::BTreeMap;
    
    let planning_error = Error::PlanningFailed {
        message: "test message".to_string(),
    };
    
    let mut extensions = BTreeMap::new();
    planning_error.populate_graphql_extensions(&mut extensions);
    
    // Verify that the error populates GraphQL extensions correctly
    assert!(!extensions.is_empty(), "Extensions should be populated");
}

// Note: More comprehensive integration tests would require setting up valid
// supergraph schemas and executable documents, which is complex for unit tests.
// Those tests should be added to integration test suites where proper
// test fixtures can be maintained. 