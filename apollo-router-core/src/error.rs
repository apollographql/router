use crate::prelude::graphql::*;
use displaydoc::Display;
use miette::{Diagnostic, NamedSource, Report, SourceSpan};
pub use router_bridge::planner::{PlanError, PlannerSetupError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::task::JoinError;
use tracing::level_filters::LevelFilter;
use typed_builder::TypedBuilder;

/// Error types for execution.
///
/// Note that these are not actually returned to the client, but are instead converted to JSON for
/// [`struct@Error`].
#[derive(Error, Display, Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[ignore_extra_doc_attributes]
pub enum FetchError {
    /// query references unknown service '{service}'
    ValidationUnknownServiceError {
        /// The service that was unknown.
        service: String,
    },

    /// invalid type for variable: '{name}'
    ValidationInvalidTypeVariable {
        /// Name of the variable.
        name: String,
    },

    /// query could not be planned: {reason}
    ValidationPlanningError {
        /// The failure reason.
        reason: String,
    },

    /// response was malformed: {reason}
    MalformedResponse {
        /// The reason the serialization failed.
        reason: String,
    },

    /// service '{service}' returned no response.
    SubrequestNoResponse {
        /// The service that returned no response.
        service: String,
    },

    /// service '{service}' response was malformed: {reason}
    SubrequestMalformedResponse {
        /// The service that responded with the malformed response.
        service: String,

        /// The reason the serialization failed.
        reason: String,
    },

    /// service '{service}' returned a PATCH response which was not expected
    SubrequestUnexpectedPatchResponse {
        /// The service that returned the PATCH response.
        service: String,
    },

    /// HTTP fetch failed from '{service}': {reason}
    ///
    /// note that this relates to a transport error and not a GraphQL error
    SubrequestHttpError {
        /// The service failed.
        service: String,

        /// The reason the fetch failed.
        reason: String,
    },

    /// subquery requires field '{field}' but it was not found in the current response
    ExecutionFieldNotFound {
        /// The field that is not found.
        field: String,
    },

    /// invalid content: {reason}
    ExecutionInvalidContent { reason: String },

    /// could not find path: {reason}
    ExecutionPathNotFound { reason: String },
}

impl FetchError {
    /// Convert the fetch error to a GraphQL error.
    pub fn to_graphql_error(&self, path: Option<Path>) -> Error {
        let value: Value = serde_json::to_value(self).unwrap().into();
        Error {
            message: self.to_string(),
            locations: Default::default(),
            path,
            extensions: value.as_object().unwrap().to_owned(),
        }
    }

    /// Convert the error to an appropriate response.
    pub fn to_response(&self) -> Response {
        Response {
            label: Default::default(),
            data: Default::default(),
            path: Default::default(),
            errors: vec![self.to_graphql_error(None)],
            extensions: Default::default(),
        }
    }
}

/// Any error.
#[derive(Error, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default, TypedBuilder)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(default))]
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

impl Error {
    pub fn from_value(service_name: &str, value: Value) -> Result<Error, FetchError> {
        let mut object =
            ensure_object!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            })?;

        let extensions =
            extract_key_value_from_object!(object, "extensions", Value::Object(o) => o)
                .map_err(|err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                })?
                .unwrap_or_default();
        let message = extract_key_value_from_object!(object, "message", Value::String(s) => s)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();
        let locations = extract_key_value_from_object!(object, "locations")
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .unwrap_or_default();
        let path = extract_key_value_from_object!(object, "path")
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?;

        Ok(Error {
            message,
            locations,
            path,
            extensions,
        })
    }
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

/// Error types for CacheResolver
#[derive(Error, Debug, Display, Clone)]
pub enum CacheResolverError {
    /// value retrieval failed: {0}
    RetrievalError(Arc<QueryPlannerError>),
}

impl From<QueryPlannerError> for CacheResolverError {
    fn from(err: QueryPlannerError) -> Self {
        CacheResolverError::RetrievalError(Arc::new(err))
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

/// Error types for service building.
#[derive(Error, Debug, Display, Clone)]
pub enum ServiceBuildError {
    /// couldn't build Router Service: {0}
    QueryPlannerError(QueryPlannerError),
}

/// Error types for QueryPlanner
#[derive(Error, Debug, Display, Clone)]
pub enum QueryPlannerError {
    /// couldn't instantiate query planner; invalid schema: {0}
    SchemaValidationErrors(PlannerSetupErrors),

    /// couldn't plan query: {0}
    PlanningErrors(PlanErrors),

    /// query planning panicked: {0}
    JoinError(Arc<JoinError>),

    /// Cache resolution failed: {0}
    CacheResolverError(Arc<CacheResolverError>),

    /// empty query plan. This often means an unhandled Introspection query was sent. Please file an issue to apollographql/router.
    EmptyPlan,

    /// unhandled planner result
    UnhandledPlannerResult,

    /// router bridge error: {0}
    RouterBridgeError(router_bridge::error::Error),
}

#[derive(Clone, Debug, Error)]
/// Container for planner setup errors
pub struct PlannerSetupErrors(Arc<Vec<PlannerSetupError>>);

impl std::fmt::Display for PlannerSetupErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "schema validation errors: {}",
            self.0
                .iter()
                .map(|e| e
                    .message
                    .clone()
                    .unwrap_or_else(|| "UNKNWON ERROR".to_string()))
                .collect::<Vec<String>>()
                .join(", ")
        ))
    }
}

impl From<Vec<PlannerSetupError>> for QueryPlannerError {
    fn from(errors: Vec<PlannerSetupError>) -> Self {
        QueryPlannerError::SchemaValidationErrors(PlannerSetupErrors(Arc::new(errors)))
    }
}

#[derive(Debug, Clone)]
pub struct PlanErrors(Arc<Vec<PlanError>>);

impl From<Vec<PlanError>> for QueryPlannerError {
    fn from(errors: Vec<PlanError>) -> Self {
        QueryPlannerError::PlanningErrors(PlanErrors(Arc::new(errors)))
    }
}

impl std::fmt::Display for PlanErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "query validation errors: {}",
            self.0
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        ))
    }
}

impl From<JoinError> for QueryPlannerError {
    fn from(err: JoinError) -> Self {
        QueryPlannerError::JoinError(Arc::new(err))
    }
}

impl From<CacheResolverError> for QueryPlannerError {
    fn from(err: CacheResolverError) -> Self {
        QueryPlannerError::CacheResolverError(Arc::new(err))
    }
}

impl From<QueryPlannerError> for Response {
    fn from(err: QueryPlannerError) -> Self {
        FetchError::from(err).to_response()
    }
}

/// Error in the schema.
#[derive(Debug, Error, Display)]
pub enum SchemaError {
    /// IO error: {0}
    IoError(#[from] std::io::Error),
    /// URL parse error for subgraph {0}: {1}
    UrlParse(String, http::uri::InvalidUri),
    /// Could not find an URL for subgraph {0}
    MissingSubgraphUrl(String),
    /// Parsing error(s).
    Parse(ParseErrors),
    /// Api error(s): {0}
    Api(String),
}

/// Collection of schema parsing errors.
#[derive(Debug)]
pub struct ParseErrors {
    pub(crate) raw_schema: String,
    pub(crate) errors: Vec<apollo_parser::Error>,
}

#[derive(Error, Debug, Diagnostic)]
#[error("{}", self.ty)]
#[diagnostic(code("apollo-parser parsing error."))]
struct ParserError {
    ty: String,
    #[source_code]
    src: NamedSource,
    #[label("{}", self.ty)]
    span: SourceSpan,
}

impl ParseErrors {
    #[allow(clippy::needless_return)]
    pub fn print(&self) {
        if LevelFilter::current() == LevelFilter::OFF {
            return;
        } else if atty::is(atty::Stream::Stdout) {
            // Fancy Miette reports for TTYs
            self.errors.iter().for_each(|err| {
                let report = Report::new(ParserError {
                    src: NamedSource::new("supergraph_schema", self.raw_schema.clone()),
                    span: (err.index(), err.data().len()).into(),
                    ty: err.message().into(),
                });
                println!("{:?}", report);
            });
        } else {
            // Best effort to display errors
            self.errors.iter().for_each(|r| {
                println!("{:#?}", r);
            });
        };
    }
}
