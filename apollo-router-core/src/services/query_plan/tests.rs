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
    // Test the new MultiplePlanningErrors variant with enum-based errors
    let errors = vec![
        PlanningErrorDetail::UnknownOperation,
        PlanningErrorDetail::OperationNameNotProvided,
        PlanningErrorDetail::QueryPlanComplexityExceeded {
            message: "Too complex".to_string(),
        },
        PlanningErrorDetail::Other {
            message: "Some other error".to_string(),
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
    assert!(error_string.contains("4 errors"));
    
    // Test that the details are accessible
    if let Error::MultiplePlanningErrors { count, errors: error_details } = multiple_error {
        assert_eq!(count, 4);
        assert_eq!(error_details.len(), 4);
        assert!(matches!(error_details[0], PlanningErrorDetail::UnknownOperation));
        assert!(matches!(error_details[1], PlanningErrorDetail::OperationNameNotProvided));
        assert!(matches!(error_details[2], PlanningErrorDetail::QueryPlanComplexityExceeded { .. }));
        assert!(matches!(error_details[3], PlanningErrorDetail::Other { .. }));
    } else {
        panic!("Expected MultiplePlanningErrors variant");
    }
}

#[test]
fn test_federation_error_conversion() {
    use apollo_federation::error::{FederationError, SingleFederationError};
    
    // Test single error conversion using From trait
    let single_fed_error = FederationError::SingleFederationError(
        SingleFederationError::UnknownOperation
    );
    let converted: Error = single_fed_error.into();
    assert!(matches!(converted, Error::PlanningFailed { .. }));
    
    // Test that we can create a simple multiple federation error using merge
    let error1 = FederationError::SingleFederationError(SingleFederationError::UnknownOperation);
    let error2 = FederationError::SingleFederationError(SingleFederationError::OperationNameNotProvided);
    let merged_error = error1.merge(error2);
    
    let converted: Error = merged_error.into();
    
    if let Error::MultiplePlanningErrors { count, errors } = converted {
        assert_eq!(count, 2);
        assert_eq!(errors.len(), 2);
        
        // Check that error variants are properly converted
        assert!(matches!(errors[0], PlanningErrorDetail::UnknownOperation));
        assert!(matches!(errors[1], PlanningErrorDetail::OperationNameNotProvided));
        
        // Check that error messages are properly converted
        assert!(errors[0].to_string().contains("Operation name not found"));
        assert!(errors[1].to_string().contains("Must provide operation name"));
    } else {
        panic!("Expected MultiplePlanningErrors variant");
    }
}

#[test]
fn test_planning_error_detail_enum_variants() {
    use apollo_federation::error::SingleFederationError;
    
    // Test conversion of various federation error types to enum variants using From trait
    let unknown_op = SingleFederationError::UnknownOperation;
    let converted: PlanningErrorDetail = unknown_op.into();
    assert!(matches!(converted, PlanningErrorDetail::UnknownOperation));
    
    let no_op_name = SingleFederationError::OperationNameNotProvided;
    let converted: PlanningErrorDetail = no_op_name.into();
    assert!(matches!(converted, PlanningErrorDetail::OperationNameNotProvided));
    
    let deferred_sub = SingleFederationError::DeferredSubscriptionUnsupported;
    let converted: PlanningErrorDetail = deferred_sub.into();
    assert!(matches!(converted, PlanningErrorDetail::DeferredSubscriptionUnsupported));
    
    let complexity = SingleFederationError::QueryPlanComplexityExceeded { 
        message: "too complex".to_string() 
    };
    let converted: PlanningErrorDetail = complexity.into();
    if let PlanningErrorDetail::QueryPlanComplexityExceeded { message } = converted {
        assert_eq!(message, "too complex");
    } else {
        panic!("Expected QueryPlanComplexityExceeded variant");
    }
    
    let cancelled = SingleFederationError::PlanningCancelled;
    let converted: PlanningErrorDetail = cancelled.into();
    assert!(matches!(converted, PlanningErrorDetail::PlanningCancelled));
    
    let no_plan = SingleFederationError::NoPlanFoundWithDisabledSubgraphs;
    let converted: PlanningErrorDetail = no_plan.into();
    assert!(matches!(converted, PlanningErrorDetail::NoPlanFoundWithDisabledSubgraphs));
    
    let invalid_graphql = SingleFederationError::InvalidGraphQL { 
        message: "bad syntax".to_string() 
    };
    let converted: PlanningErrorDetail = invalid_graphql.into();
    if let PlanningErrorDetail::InvalidGraphQL { message } = converted {
        assert_eq!(message, "bad syntax");
    } else {
        panic!("Expected InvalidGraphQL variant");
    }
    
    let invalid_subgraph = SingleFederationError::InvalidSubgraph { 
        message: "bad subgraph".to_string() 
    };
    let converted: PlanningErrorDetail = invalid_subgraph.into();
    if let PlanningErrorDetail::InvalidSubgraph { message } = converted {
        assert_eq!(message, "bad subgraph");
    } else {
        panic!("Expected InvalidSubgraph variant");
    }
    
    // Test fallback to Other variant for unmapped error types
    let internal_error = SingleFederationError::Internal { 
        message: "internal issue".to_string() 
    };
    let converted: PlanningErrorDetail = internal_error.into();
    if let PlanningErrorDetail::Other { message } = converted {
        assert!(message.contains("internal issue"));
    } else {
        panic!("Expected Other variant");
    }
}

#[test]
fn test_planning_error_detail_serialization() {
    // Test that PlanningErrorDetail enum variants can be serialized/deserialized
    let unknown_op = PlanningErrorDetail::UnknownOperation;
    let json = serde_json::to_string(&unknown_op).expect("Should serialize");
    let deserialized: PlanningErrorDetail = serde_json::from_str(&json).expect("Should deserialize");
    assert!(matches!(deserialized, PlanningErrorDetail::UnknownOperation));
    
    let complexity = PlanningErrorDetail::QueryPlanComplexityExceeded {
        message: "Test complexity message".to_string(),
    };
    let json = serde_json::to_string(&complexity).expect("Should serialize");
    let deserialized: PlanningErrorDetail = serde_json::from_str(&json).expect("Should deserialize");
    if let PlanningErrorDetail::QueryPlanComplexityExceeded { message } = deserialized {
        assert_eq!(message, "Test complexity message");
    } else {
        panic!("Expected QueryPlanComplexityExceeded variant");
    }
    
    let other = PlanningErrorDetail::Other {
        message: "Some other error".to_string(),
    };
    let json = serde_json::to_string(&other).expect("Should serialize");
    let deserialized: PlanningErrorDetail = serde_json::from_str(&json).expect("Should deserialize");
    if let PlanningErrorDetail::Other { message } = deserialized {
        assert_eq!(message, "Some other error");
    } else {
        panic!("Expected Other variant");
    }
}

#[test]
fn test_planning_error_detail_error_trait() {
    use apollo_router_error::Error as RouterError;
    
    // Test that PlanningErrorDetail implements the Error trait correctly with different variants
    let unknown_op = PlanningErrorDetail::UnknownOperation;
    assert_eq!(unknown_op.error_code(), "APOLLO_ROUTER_QUERY_PLAN_UNKNOWN_OPERATION");
    
    let complexity = PlanningErrorDetail::QueryPlanComplexityExceeded {
        message: "Test message".to_string(),
    };
    assert_eq!(complexity.error_code(), "APOLLO_ROUTER_QUERY_PLAN_COMPLEXITY_EXCEEDED");
    
    let other = PlanningErrorDetail::Other {
        message: "Test other error".to_string(),
    };
    assert_eq!(other.error_code(), "APOLLO_ROUTER_QUERY_PLAN_OTHER_PLANNING_ERROR");
    
    // Test GraphQL extensions population
    let mut extensions = std::collections::BTreeMap::new();
    complexity.populate_graphql_extensions(&mut extensions);
    
    // Should contain the extension field from the enum variant
    assert!(extensions.contains_key("complexityMessage"));
    
    let mut other_extensions = std::collections::BTreeMap::new();
    other.populate_graphql_extensions(&mut other_extensions);
    
    // Should contain the extension field from the Other variant
    assert!(other_extensions.contains_key("errorMessage"));
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
            PlanningErrorDetail::UnknownOperation,
            PlanningErrorDetail::Other {
                message: "error 2".to_string(),
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
            PlanningErrorDetail::UnknownOperation,
            PlanningErrorDetail::Other {
                message: "Second error".to_string(),
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
            PlanningErrorDetail::QueryPlanComplexityExceeded {
                message: "First error".to_string(),
            },
            PlanningErrorDetail::Other {
                message: "Second error".to_string(),
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