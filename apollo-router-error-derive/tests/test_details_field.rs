//! Test that verifies fields named 'details' work correctly with the derive macro

use apollo_router_error_derive::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum TestError {
    /// Request serialization failed
    #[error("Request serialization failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_TEST_REQUEST_SERIALIZATION_ERROR),
        help("Check that the request can be properly serialized")
    )]
    RequestSerialization {
        #[extension("context")]
        context: String,
        #[extension("details")]
        details: String, // This should not cause naming collision anymore
        #[extension("errorCode")]
        error_code: u32,
    },
}

#[test]
fn test_details_field_no_collision() {
    let error = TestError::RequestSerialization {
        context: "test context".to_string(),
        details: "test details".to_string(),
        error_code: 42,
    };

    // This should work without compilation errors
    let mut extensions = std::collections::BTreeMap::<String, serde_json::Value>::new();
    apollo_router_error::Error::populate_graphql_extensions(&error, &mut extensions);

    // Verify the extensions were populated correctly
    assert_eq!(
        extensions.get("context").unwrap().as_str().unwrap(),
        "test context"
    );
    assert_eq!(
        extensions.get("details").unwrap().as_str().unwrap(),
        "test details"
    );
    assert_eq!(extensions.get("errorCode").unwrap().as_u64().unwrap(), 42);
    assert_eq!(
        extensions.get("errorType").unwrap().as_str().unwrap(),
        "REQUEST_SERIALIZATION"
    );
}
