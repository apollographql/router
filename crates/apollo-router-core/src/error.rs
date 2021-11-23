use crate::prelude::graphql::*;
use displaydoc::Display;
pub use router_bridge::plan::PlanningErrors;
use serde::{Deserialize, Serialize};
use std::sync::mpsc::RecvError;
use std::sync::Arc;
use thiserror::Error;
use tokio::task::JoinError;

/// Error types for execution.
///
/// Note that these are not actually returned to the client, but are instead converted to JSON for
/// [`struct@Error`].
#[derive(Error, Display, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
#[ignore_extra_doc_attributes]
pub enum FetchError {
    /// Query references unknown service '{service}'.
    ValidationUnknownServiceError {
        /// The service that was unknown.
        service: String,
    },

    /// Query requires variable '{name}', but it was not provided.
    ValidationMissingVariable {
        /// Name of the variable.
        name: String,
    },

    /// Query could not be planned: {reason}
    ValidationPlanningError {
        /// The failure reason.
        reason: String,
    },

    /// Response was malformed: {reason}
    MalformedResponse {
        /// The reason the serialization failed.
        reason: String,
    },

    /// Service '{service}' returned no response.
    SubrequestNoResponse {
        /// The service that returned no response.
        service: String,
    },

    /// Service '{service}' response was malformed: {reason}
    SubrequestMalformedResponse {
        /// The service that responded with the malformed response.
        service: String,

        /// The reason the serialization failed.
        reason: String,
    },

    /// Service '{service}' returned a PATCH response which was not expected.
    SubrequestUnexpectedPatchResponse {
        /// The service that returned the PATCH response.
        service: String,
    },

    /// HTTP fetch failed from '{service}': {reason}
    ///
    /// Note that this relates to a transport error and not a GraphQL error.
    SubrequestHttpError {
        /// The service failed.
        service: String,

        /// The reason the fetch failed.
        reason: String,
    },

    /// Subquery requires field '{field}' but it was not found in the current response.
    ExecutionFieldNotFound {
        /// The field that is not found.
        field: String,
    },

    /// Invalid content: {reason}
    ExecutionInvalidContent { reason: String },

    /// Could not find path: {reason}
    ExecutionPathNotFound { reason: String },
}

impl FetchError {
    /// Convert the fetch error to a GraphQL error.
    pub fn to_graphql_error(&self, path: Option<Path>) -> Error {
        Error {
            message: self.to_string(),
            locations: Default::default(),
            path,
            extensions: serde_json::to_value(self)
                .unwrap()
                .as_object()
                .unwrap()
                .to_owned(),
        }
    }

    /// Convert the error to an appropriate response.
    pub fn to_response(&self, primary: bool) -> Response {
        Response {
            label: Default::default(),
            data: Default::default(),
            path: Default::default(),
            has_next: primary.then(|| false),
            errors: vec![self.to_graphql_error(None)],
            extensions: Default::default(),
        }
    }
}

/// Any error.
#[derive(Error, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct Error {
    /// The error message.
    pub message: String,

    /// The locations of the error from the originating request.
    pub locations: Vec<Location>,

    /// The path of the error.
    pub path: Option<Path>,

    /// The optional graphql extensions.
    #[serde(default, skip_serializing_if = "Object::is_empty")]
    pub extensions: Object,
}

/// A location in the request that triggered a graphql error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// The line number.
    pub line: i32,

    /// The column number.
    pub column: i32,
}

impl From<QueryPlannerError> for FetchError {
    fn from(err: QueryPlannerError) -> Self {
        FetchError::ValidationPlanningError {
            reason: err.to_string(),
        }
    }
}

/// An error while processing JSON data.
#[derive(Debug, Error, Display)]
pub enum JsonExtError {
    /// Could not find path in JSON.
    PathNotFound,
    /// Attempt to flatten on non-array node.
    InvalidFlatten,
}

/// Error types for QueryPlanner
#[derive(Error, Debug, Display, Clone)]
pub enum QueryPlannerError {
    /// Query planning had errors: {0}
    PlanningErrors(Arc<PlanningErrors>),

    /// Query planning panicked: {0}
    JoinError(Arc<JoinError>),

    /// Query planning cache failed: {0}
    CacheError(RecvError),
}

impl From<PlanningErrors> for QueryPlannerError {
    fn from(err: PlanningErrors) -> Self {
        QueryPlannerError::PlanningErrors(Arc::new(err))
    }
}

impl From<JoinError> for QueryPlannerError {
    fn from(err: JoinError) -> Self {
        QueryPlannerError::JoinError(Arc::new(err))
    }
}

/// Error in the schema.
#[derive(Debug, Error, Display)]
pub enum SchemaError {
    /// IO error: {0}
    IoError(#[from] std::io::Error),
    /// Parsing error(s).
    ParseErrors(Vec<apollo_parser::Error>),
}
