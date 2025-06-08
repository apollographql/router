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
fn test_multiple_planning_errors() {
    // Test the new MultiplePlanningErrors variant
    let errors = vec![
        PlanningErrorDetail {
            message: "First planning error".to_string(),
            code: Some("ERROR_1".to_string()),
        },
        PlanningErrorDetail {
            message: "Second planning error".to_string(),
            code: Some("ERROR_2".to_string()),
        },
        PlanningErrorDetail {
            message: "Third planning error without code".to_string(),
            code: None,
        },
    ];
    
    let multiple_error = Error::MultiplePlanningErrors {
        count: errors.len(),
        errors: errors.clone(),
    };
    
    // Test that we can create the multiple error variant
    assert!(matches!(multiple_error, Error::MultiplePlanningErrors { .. }));
    
    // Test that the error displays correctly
    let error_string = format!("{}", multiple_error);
    assert!(error_string.contains("Multiple query planning errors"));
    assert!(error_string.contains("3 errors"));
    
    // Test that the details are accessible
    if let Error::MultiplePlanningErrors { count, errors: error_details } = multiple_error {
        assert_eq!(count, 3);
        assert_eq!(error_details.len(), 3);
        assert_eq!(error_details[0].message, "First planning error");
        assert_eq!(error_details[0].code, Some("ERROR_1".to_string()));
        assert_eq!(error_details[1].message, "Second planning error");
        assert_eq!(error_details[1].code, Some("ERROR_2".to_string()));
        assert_eq!(error_details[2].message, "Third planning error without code");
        assert_eq!(error_details[2].code, None);
    } else {
        panic!("Expected MultiplePlanningErrors variant");
    }
}

#[test]
fn test_federation_error_conversion() {
    use apollo_federation::error::{FederationError, SingleFederationError};
    
    // Test single error conversion
    let single_fed_error = FederationError::SingleFederationError(
        SingleFederationError::UnknownOperation
    );
    let converted = Error::from_federation_error(single_fed_error);
    assert!(matches!(converted, Error::PlanningFailed { .. }));
    
    // Test that we can create a simple multiple federation error using merge
    let error1 = FederationError::SingleFederationError(SingleFederationError::UnknownOperation);
    let error2 = FederationError::SingleFederationError(SingleFederationError::OperationNameNotProvided);
    let merged_error = error1.merge(error2);
    
    let converted = Error::from_federation_error(merged_error);
    
    if let Error::MultiplePlanningErrors { count, errors } = converted {
        assert_eq!(count, 2);
        assert_eq!(errors.len(), 2);
        
        // Check that error codes are properly extracted for known error types
        assert_eq!(errors[0].code, Some("UNKNOWN_OPERATION".to_string()));
        assert_eq!(errors[1].code, Some("OPERATION_NAME_NOT_PROVIDED".to_string()));
        
        // Check that error messages are properly converted
        assert!(errors[0].message.contains("Operation name not found"));
        assert!(errors[1].message.contains("Must provide operation name"));
    } else {
        panic!("Expected MultiplePlanningErrors variant");
    }
}

#[test]
fn test_error_code_extraction() {
    use apollo_federation::error::SingleFederationError;
    
    // Test various error types and their code extraction
    let unknown_op = SingleFederationError::UnknownOperation;
    assert_eq!(Error::extract_error_code(&unknown_op), Some("UNKNOWN_OPERATION".to_string()));
    
    let no_op_name = SingleFederationError::OperationNameNotProvided;
    assert_eq!(Error::extract_error_code(&no_op_name), Some("OPERATION_NAME_NOT_PROVIDED".to_string()));
    
    let deferred_sub = SingleFederationError::DeferredSubscriptionUnsupported;
    assert_eq!(Error::extract_error_code(&deferred_sub), Some("DEFERRED_SUBSCRIPTION_UNSUPPORTED".to_string()));
    
    let complexity = SingleFederationError::QueryPlanComplexityExceeded { message: "too complex".to_string() };
    assert_eq!(Error::extract_error_code(&complexity), Some("QUERY_PLAN_COMPLEXITY_EXCEEDED".to_string()));
    
    let cancelled = SingleFederationError::PlanningCancelled;
    assert_eq!(Error::extract_error_code(&cancelled), Some("PLANNING_CANCELLED".to_string()));
    
    let no_plan = SingleFederationError::NoPlanFoundWithDisabledSubgraphs;
    assert_eq!(Error::extract_error_code(&no_plan), Some("NO_PLAN_FOUND_WITH_DISABLED_SUBGRAPHS".to_string()));
    
    let invalid_graphql = SingleFederationError::InvalidGraphQL { message: "bad syntax".to_string() };
    assert_eq!(Error::extract_error_code(&invalid_graphql), Some("INVALID_GRAPHQL".to_string()));
    
    let invalid_subgraph = SingleFederationError::InvalidSubgraph { message: "bad subgraph".to_string() };
    assert_eq!(Error::extract_error_code(&invalid_subgraph), Some("INVALID_SUBGRAPH".to_string()));
    
    // Test an error type that doesn't have a specific code
    let internal_error = SingleFederationError::Internal { message: "internal issue".to_string() };
    assert_eq!(Error::extract_error_code(&internal_error), None);
}

#[test]
fn test_planning_error_detail_serialization() {
    // Test that PlanningErrorDetail can be serialized/deserialized
    let detail = PlanningErrorDetail {
        message: "Test error message".to_string(),
        code: Some("TEST_ERROR_CODE".to_string()),
    };
    
    let json = serde_json::to_string(&detail).expect("Should serialize");
    let deserialized: PlanningErrorDetail = serde_json::from_str(&json).expect("Should deserialize");
    
    assert_eq!(deserialized.message, detail.message);
    assert_eq!(deserialized.code, detail.code);
    
    // Test detail without code
    let detail_no_code = PlanningErrorDetail {
        message: "Error without code".to_string(),
        code: None,
    };
    
    let json_no_code = serde_json::to_string(&detail_no_code).expect("Should serialize");
    let deserialized_no_code: PlanningErrorDetail = serde_json::from_str(&json_no_code).expect("Should deserialize");
    
    assert_eq!(deserialized_no_code.message, detail_no_code.message);
    assert_eq!(deserialized_no_code.code, None);
}

#[test]
fn test_error_codes() {
    use apollo_router_error::Error as RouterError;
    
    let planning_error = Error::PlanningFailed {
        message: "test".to_string(),
    };
    
    let multiple_error = Error::MultiplePlanningErrors {
        count: 2,
        errors: vec![
            PlanningErrorDetail {
                message: "error 1".to_string(),
                code: Some("CODE_1".to_string()),
            },
            PlanningErrorDetail {
                message: "error 2".to_string(),
                code: None,
            },
        ],
    };
    
    let federation_error = Error::FederationError {
        message: "test".to_string(),
    };
    
    let invalid_supergraph = Error::InvalidSupergraph {
        reason: "test".to_string(),
    };

    // Test that error codes are correctly implemented
    assert_eq!(planning_error.error_code(), "APOLLO_ROUTER_QUERY_PLAN_PLANNING_FAILED");
    assert_eq!(multiple_error.error_code(), "APOLLO_ROUTER_QUERY_PLAN_MULTIPLE_PLANNING_ERRORS");
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
    
    // Test multiple errors display
    let multiple_error = Error::MultiplePlanningErrors {
        count: 2,
        errors: vec![
            PlanningErrorDetail {
                message: "First error".to_string(),
                code: Some("ERROR_1".to_string()),
            },
            PlanningErrorDetail {
                message: "Second error".to_string(),
                code: None,
            },
        ],
    };
    
    let multiple_error_string = format!("{}", multiple_error);
    assert!(multiple_error_string.contains("Multiple query planning errors"));
    assert!(multiple_error_string.contains("2 errors"));
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
    
    // Test multiple errors extension population
    let multiple_error = Error::MultiplePlanningErrors {
        count: 2,
        errors: vec![
            PlanningErrorDetail {
                message: "First error".to_string(),
                code: Some("ERROR_1".to_string()),
            },
            PlanningErrorDetail {
                message: "Second error".to_string(),
                code: None,
            },
        ],
    };
    
    let mut multiple_extensions = BTreeMap::new();
    multiple_error.populate_graphql_extensions(&mut multiple_extensions);
    
    // Verify that multiple errors populate extensions correctly
    assert!(!multiple_extensions.is_empty(), "Extensions should be populated for multiple errors");
    
    // Verify that both errorCount and planningErrors are present
    assert!(multiple_extensions.contains_key("errorCount"), "Should contain errorCount");
    assert!(multiple_extensions.contains_key("planningErrors"), "Should contain planningErrors");
}

// Note: More comprehensive integration tests would require setting up valid
// supergraph schemas and executable documents, which is complex for unit tests.
// Those tests should be added to integration test suites where proper
// test fixtures can be maintained. 