use apollo_federation::merge::{MergeFailure, MergeSuccess};

/// Assert that composition succeeded without errors
pub fn assert_composition_success(result: &Result<MergeSuccess, MergeFailure>) {
    match result {
        Ok(success) => {
            // Validate that the schema is valid
            let schema = success.schema.clone().into_inner();
            let validation = schema.validate();
            assert!(validation.is_ok(), "Schema validation failed: {:?}", validation);
        }
        Err(failure) => {
            panic!(
                "Expected composition to succeed, but got errors: {:?}",
                failure.errors
            );
        }
    }
}

/// Assert that composition failed with specific error patterns
pub fn assert_composition_failure(
    result: &Result<MergeSuccess, MergeFailure>,
    expected_error_patterns: &[&str],
) {
    match result {
        Ok(_) => {
            panic!("Expected composition to fail, but it succeeded");
        }
        Err(failure) => {
            for pattern in expected_error_patterns {
                let found = failure.errors.iter().any(|error| error.contains(pattern));
                assert!(
                    found,
                    "Expected error pattern '{}' not found in errors: {:?}",
                    pattern,
                    failure.errors
                );
            }
        }
    }
}

/// Assert that the schema contains specific content
pub fn assert_schema_contains(schema_sdl: &str, expected_content: &str) {
    assert!(
        schema_sdl.contains(expected_content),
        "Schema does not contain expected content '{}'\nSchema:\n{}",
        expected_content,
        schema_sdl
    );
}

/// Assert that the schema does not contain specific content
pub fn assert_schema_not_contains(schema_sdl: &str, unexpected_content: &str) {
    assert!(
        !schema_sdl.contains(unexpected_content),
        "Schema contains unexpected content '{}'\nSchema:\n{}",
        unexpected_content,
        schema_sdl
    );
}

/// Extract error information from a failed composition result
pub fn extract_errors(result: &Result<MergeSuccess, MergeFailure>) -> Vec<String> {
    match result {
        Ok(_) => vec![],
        Err(failure) => failure.errors.clone(),
    }
}

/// Extract composition hints from a result
pub fn extract_hints(result: &Result<MergeSuccess, MergeFailure>) -> Vec<String> {
    match result {
        Ok(success) => success.composition_hints.clone(),
        Err(failure) => failure.composition_hints.clone(),
    }
}

/// Macro for asserting composition success with cleaner syntax
#[macro_export]
macro_rules! assert_composition_success {
    ($result:expr) => {
        $crate::test_helpers::assert_composition_success(&$result)
    };
}

/// Macro for asserting composition failure with error patterns
#[macro_export]
macro_rules! assert_composition_failure {
    ($result:expr, $($pattern:expr),+) => {
        $crate::test_helpers::assert_composition_failure(&$result, &[$($pattern),+])
    };
}