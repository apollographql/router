pub mod tower_test;
pub use tower_test::TowerTest;

/// Assert that a Result contains a specific error variant
///
/// This macro handles the common pattern of downcasting a BoxError to a specific
/// error type and then pattern matching on the expected variant.
///
/// # Examples
///
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
#[macro_export]
macro_rules! assert_error {
    ($result:expr, $error_type:ty, $pattern:pat) => {
        if let Err(error) = $result {
            if let Some(typed_error) = error.downcast_ref::<$error_type>() {
                assert!(
                    matches!(typed_error, $pattern),
                    "Error variant mismatch. Expected pattern {}, got {:?}",
                    stringify!($pattern),
                    typed_error
                );
            } else {
                panic!(
                    "Failed to downcast error to {}. Got: {}",
                    stringify!($error_type),
                    error
                );
            }
        } else {
            panic!("Expected error result, got Ok");
        }
    };

    // Convenience version that just checks the error type without variant matching
    ($result:expr, $error_type:ty) => {
        if let Err(error) = $result {
            if error.downcast_ref::<$error_type>().is_none() {
                panic!(
                    "Failed to downcast error to {}. Got: {}",
                    stringify!($error_type),
                    error
                );
            }
        } else {
            panic!("Expected error result, got Ok");
        }
    };
}
