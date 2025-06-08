pub mod tower_test;
pub use tower_test::TowerTest;

/// Assert that a Result contains a specific error variant
///
/// This macro handles the common pattern of downcasting a BoxError to a specific
/// error type and then pattern matching on the expected variant.
///
/// # Examples
///
/// Basic pattern matching:
/// ```rust
/// use crate::test_utils::assert_error;
/// use crate::layers::bytes_to_json::Error as BytesToJsonError;
///
/// let result: Result<(), tower::BoxError> = Err(BytesToJsonError::JsonDeserialization {
///     json_error: serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
///     input_data: Some("invalid".to_string()),
///     error_position: None,
/// }.into());
///
/// assert_error!(result, BytesToJsonError, BytesToJsonError::JsonDeserialization { .. });
/// ```
///
/// Field validation with custom assertions (BoxError):
/// ```rust
/// use crate::test_utils::assert_error;
/// use crate::services::query_preparation::Error;
///
/// let result: Result<(), tower::BoxError> = Err(Error::JsonExtraction {
///     field: "query".to_string(),
///     message: "Missing field".to_string(),
/// }.into());
///
/// assert_error!(result, Error, Error::JsonExtraction { field, .. } => {
///     assert_eq!(field, "query");
/// });
/// ```
///
/// Direct pattern matching (concrete error types):
/// ```rust
/// use crate::test_utils::assert_error;
/// use crate::services::query_preparation::Error;
///
/// let result: Result<(), Error> = Err(Error::JsonExtraction {
///     field: "query".to_string(),
///     message: "Missing field".to_string(),
/// });
///
/// assert_error!(result, Error::JsonExtraction { field, .. } => {
///     assert_eq!(field, "query");
/// });
/// ```
#[macro_export]
macro_rules! assert_error {
    // Direct pattern matching with field validation (concrete error types)
    ($result:expr, $pattern:pat => $validation:block) => {
        if let Err(error) = &$result {
            match error {
                $pattern => $validation,
                other => panic!(
                    "Error variant mismatch. Expected pattern {}, got {:?}",
                    stringify!($pattern),
                    other
                ),
            }
        } else {
            panic!("Expected error result, got Ok");
        }
    };

    // Direct pattern matching (concrete error types)
    ($result:expr, $pattern:pat) => {
        if let Err(error) = &$result {
            assert!(
                matches!(error, $pattern),
                "Error variant mismatch. Expected pattern {}, got {:?}",
                stringify!($pattern),
                error
            );
        } else {
            panic!("Expected error result, got Ok");
        }
    };

    // BoxError downcasting with field validation
    ($result:expr, $error_type:ty, $pattern:pat => $validation:block) => {
        if let Err(error) = &$result {
            if let Some(typed_error) = error.downcast_ref::<$error_type>() {
                match typed_error {
                    $pattern => $validation,
                    other => panic!(
                        "Error variant mismatch. Expected pattern {}, got {:?}",
                        stringify!($pattern),
                        other
                    ),
                }
            } else {
                panic!("Failed to downcast error to {}", stringify!($error_type));
            }
        } else {
            panic!("Expected error result, got Ok");
        }
    };

    // BoxError downcasting with basic pattern matching
    ($result:expr, $error_type:ty, $pattern:pat) => {
        if let Err(error) = &$result {
            if let Some(typed_error) = error.downcast_ref::<$error_type>() {
                assert!(
                    matches!(typed_error, $pattern),
                    "Error variant mismatch. Expected pattern {}, got {:?}",
                    stringify!($pattern),
                    typed_error
                );
            } else {
                panic!("Failed to downcast error to {}", stringify!($error_type));
            }
        } else {
            panic!("Expected error result, got Ok");
        }
    };

    // BoxError type checking only
    ($result:expr, $error_type:ty) => {
        if let Err(ref error) = &$result {
            if error.downcast_ref::<$error_type>().is_none() {
                panic!("Failed to downcast error to {}", stringify!($error_type));
            }
        } else {
            panic!("Expected error result, got Ok");
        }
    };
}
