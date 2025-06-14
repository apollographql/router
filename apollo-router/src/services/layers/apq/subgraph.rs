//! APQ (Automatic Persisted Queries) integration for subgraph services.
//!
//! This module contains subgraph-specific APQ logic including error detection
//! and retry handling for APQ requests to subgraphs.

use crate::graphql;

/// APQ error variants that can be returned from subgraph responses
#[derive(Debug, PartialEq)]
pub(crate) enum APQError {
    /// The subgraph does not support persisted queries
    PersistedQueryNotSupported,
    /// The persisted query was not found in the subgraph's cache
    PersistedQueryNotFound,
    /// Other error or no APQ error detected
    Other,
}

/// Constants for APQ error detection
pub(crate) const PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE: &str = "PERSISTED_QUERY_NOT_FOUND";
pub(crate) const PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE: &str =
    "PERSISTED_QUERY_NOT_SUPPORTED";
pub(crate) const PERSISTED_QUERY_NOT_FOUND_MESSAGE: &str = "PersistedQueryNotFound";
pub(crate) const PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE: &str = "PersistedQueryNotSupported";
const CODE_STRING: &str = "code";

/// Analyzes a GraphQL response to detect APQ-related errors
///
/// This function examines both the error message and extensions to identify
/// if the response contains an APQ error that requires special handling.
///
/// # Arguments
///
/// * `gql_response` - The GraphQL response to analyze
///
/// # Returns
///
/// An `APQError` variant indicating the type of APQ error detected, or `Other`
/// if no APQ error was found.
pub(crate) fn get_apq_error(gql_response: &graphql::Response) -> APQError {
    for error in &gql_response.errors {
        // Check if error message is an APQ error
        match error.message.as_str() {
            PERSISTED_QUERY_NOT_FOUND_MESSAGE => {
                return APQError::PersistedQueryNotFound;
            }
            PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE => {
                return APQError::PersistedQueryNotSupported;
            }
            _ => {}
        }
        // Check if extensions contains the APQ error in "code"
        if let Some(value) = error.extensions.get(CODE_STRING) {
            if value == PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE {
                return APQError::PersistedQueryNotFound;
            } else if value == PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE {
                return APQError::PersistedQueryNotSupported;
            }
        }
    }
    APQError::Other
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::ByteString;

    use super::*;
    use crate::graphql::Error;
    use crate::graphql::Response;
    use crate::json_ext::Object;
    use crate::json_ext::Value;

    #[test]
    fn test_get_apq_error_not_found_message() {
        let mut response = Response::default();
        response.errors.push(Error {
            message: "PersistedQueryNotFound".to_string(),
            ..Error::default()
        });

        assert_eq!(get_apq_error(&response), APQError::PersistedQueryNotFound);
    }

    #[test]
    fn test_get_apq_error_not_supported_message() {
        let mut response = Response::default();
        response.errors.push(Error {
            message: "PersistedQueryNotSupported".to_string(),
            ..Error::default()
        });

        assert_eq!(
            get_apq_error(&response),
            APQError::PersistedQueryNotSupported
        );
    }

    #[test]
    fn test_get_apq_error_not_found_extension() {
        let mut extensions = Object::new();
        extensions.insert(
            "code".to_string(),
            Value::String(ByteString::from("PERSISTED_QUERY_NOT_FOUND")),
        );

        let mut response = Response::default();
        response.errors.push(Error {
            message: "Some error".to_string(),
            extensions,
            ..Error::default()
        });

        assert_eq!(get_apq_error(&response), APQError::PersistedQueryNotFound);
    }

    #[test]
    fn test_get_apq_error_not_supported_extension() {
        let mut extensions = Object::new();
        extensions.insert(
            "code".to_string(),
            Value::String(ByteString::from("PERSISTED_QUERY_NOT_SUPPORTED")),
        );

        let mut response = Response::default();
        response.errors.push(Error {
            message: "Some error".to_string(),
            extensions,
            ..Error::default()
        });

        assert_eq!(
            get_apq_error(&response),
            APQError::PersistedQueryNotSupported
        );
    }

    #[test]
    fn test_get_apq_error_other() {
        let mut response = Response::default();
        response.errors.push(Error {
            message: "Some other error".to_string(),
            ..Error::default()
        });

        assert_eq!(get_apq_error(&response), APQError::Other);
    }

    #[test]
    fn test_get_apq_error_no_errors() {
        let response = Response::default();
        assert_eq!(get_apq_error(&response), APQError::Other);
    }
}
