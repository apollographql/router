use apollo_errors::Error;
use apollo_errors::miette;

/// Errors returned when parsing or mutating a JSON document.
#[derive(Debug, Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum JsonError {
    /// The input is not a valid JSON document.
    #[error("JSON syntax error at byte {offset}: {reason}")]
    #[diagnostic(code(apollo_json::syntax))]
    #[http_status(400)]
    Syntax {
        /// Byte offset into the input where parsing failed.
        offset: usize,
        /// Short description of what was expected or found.
        reason: &'static str,
    },

    /// Container nesting exceeds the configured maximum depth.
    #[error("JSON nesting depth exceeds the configured limit of {limit}")]
    #[diagnostic(code(apollo_json::depth_limit))]
    #[http_status(400)]
    DepthLimitExceeded {
        /// The configured depth limit.
        limit: usize,
    },

    /// Parsing the document would exceed the configured arena size limit.
    #[error("parsed document exceeds the configured arena size limit of {limit} bytes")]
    #[diagnostic(code(apollo_json::arena_limit))]
    #[http_status(413)]
    ArenaLimitExceeded {
        /// The configured arena size limit in bytes.
        limit: usize,
    },

    /// A non-finite float cannot be represented in JSON.
    #[error("non-finite numbers cannot be represented in JSON")]
    #[diagnostic(code(apollo_json::non_finite_number))]
    #[http_status(400)]
    NonFiniteNumber,

    /// A mutation path does not resolve to an existing location.
    #[error("path segment {segment} does not resolve in the document")]
    #[diagnostic(code(apollo_json::path_not_found))]
    #[http_status(400)]
    PathNotFound {
        /// Index of the path segment that failed to resolve.
        segment: usize,
    },

    /// Typed deserialization failed: the document's shape does not match the
    /// target type.
    #[error("{message}")]
    #[diagnostic(code(apollo_json::deserialization))]
    #[http_status(400)]
    Deserialization {
        /// What was expected and what was found, with the byte offset where
        /// available.
        message: String,
    },

    /// Building a document from a `Serialize` type failed.
    #[error("{message}")]
    #[diagnostic(code(apollo_json::serialization))]
    #[http_status(500)]
    Serialization {
        /// What the value's `Serialize` implementation produced that a JSON
        /// document cannot represent.
        message: String,
    },
}

impl serde::de::Error for JsonError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        JsonError::Deserialization {
            message: msg.to_string(),
        }
    }
}

impl serde::ser::Error for JsonError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        JsonError::Serialization {
            message: msg.to_string(),
        }
    }
}
