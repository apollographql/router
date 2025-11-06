//! Router errors.
use std::ops::Deref;
use std::sync::Arc;

use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::validation::WithErrors;
use apollo_federation::error::FederationError;
use displaydoc::Display;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tokio::task::JoinError;
use tower::BoxError;

use crate::apollo_studio_interop::UsageReporting;
pub(crate) use crate::configuration::ConfigurationError;
pub(crate) use crate::graphql::Error;
use crate::graphql::ErrorExtension;
use crate::graphql::IntoGraphQLErrors;
use crate::graphql::Location as ErrorLocation;
use crate::graphql::Response;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::spec::SpecError;
use crate::spec::operation_limits::OperationLimits;

/// Return up to this many GraphQL parsing or validation errors.
///
/// Any remaining errors get silently dropped.
const MAX_VALIDATION_ERRORS: usize = 100;

/// Error types for execution.
///
/// Note that these are not actually returned to the client, but are instead converted to JSON for
/// [`struct@Error`].
#[derive(Error, Display, Debug, Clone, Serialize, Eq, PartialEq)]
#[serde(untagged)]
#[ignore_extra_doc_attributes]
#[non_exhaustive]
#[allow(missing_docs)] // FIXME
pub(crate) enum FetchError {
    /// {message}
    ValidationInvalidTypeVariable {
        name: serde_json_bytes::ByteString,
        message: crate::spec::InvalidInputValue,
    },

    /// query could not be planned: {reason}
    ValidationPlanningError {
        /// The failure reason.
        reason: String,
    },

    /// request was malformed: {reason}
    MalformedRequest {
        /// The reason the serialization failed.
        reason: String,
    },

    /// response was malformed: {reason}
    MalformedResponse {
        /// The reason the serialization failed.
        reason: String,
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
        status_code: Option<u16>,

        /// The service failed.
        service: String,

        /// The reason the fetch failed.
        reason: String,
    },
    /// Websocket fetch failed from '{service}': {reason}
    ///
    /// note that this relates to a transport error and not a GraphQL error
    SubrequestWsError {
        /// The service failed.
        service: String,

        /// The reason the fetch failed.
        reason: String,
    },

    /// could not find path: {reason}
    ExecutionPathNotFound { reason: String },

    /// Batching error for '{service}': {reason}
    SubrequestBatchingError {
        /// The service for which batch processing failed.
        service: String,

        /// The reason batch processing failed.
        reason: String,
    },
}

impl FetchError {
    /// Convert the fetch error to a GraphQL error.
    pub(crate) fn to_graphql_error(&self, path: Option<Path>) -> Error {
        // FIXME(SimonSapin): this causes every Rust field to be included in `extensions`,
        // do we really want that?
        let mut value: Value = serde_json_bytes::to_value(self).unwrap_or_default();
        if let Some(extensions) = value.as_object_mut() {
            extensions
                .entry("code")
                .or_insert_with(|| self.extension_code().into());
            // Following these specs https://www.apollographql.com/docs/apollo-server/data/errors/#including-custom-error-details
            match self {
                FetchError::SubrequestHttpError {
                    service,
                    status_code,
                    ..
                } => {
                    extensions
                        .entry("service")
                        .or_insert_with(|| service.clone().into());
                    extensions.remove("status_code");
                    if let Some(status_code) = status_code {
                        extensions
                            .insert("http", serde_json_bytes::json!({ "status": status_code }));
                    }
                }
                FetchError::SubrequestMalformedResponse { service, .. }
                | FetchError::SubrequestUnexpectedPatchResponse { service }
                | FetchError::SubrequestWsError { service, .. } => {
                    extensions
                        .entry("service")
                        .or_insert_with(|| service.clone().into());
                }
                FetchError::ValidationInvalidTypeVariable { name, .. } => {
                    extensions.remove("message");
                    extensions
                        .entry("name")
                        .or_insert_with(|| Value::String(name.clone()));
                }
                _ => (),
            }
        }

        Error::builder()
            .message(self.to_string())
            .locations(Vec::default())
            .and_path(path)
            .extensions(value.as_object().unwrap().to_owned())
            .build()
    }

    /// Convert the error to an appropriate response.
    pub(crate) fn to_response(&self) -> Response {
        Response {
            errors: vec![self.to_graphql_error(None)],
            ..Response::default()
        }
    }
}

impl ErrorExtension for FetchError {
    fn extension_code(&self) -> String {
        match self {
            FetchError::ValidationInvalidTypeVariable { .. } => "VALIDATION_INVALID_TYPE_VARIABLE",
            FetchError::ValidationPlanningError { .. } => "VALIDATION_PLANNING_ERROR",
            FetchError::SubrequestMalformedResponse { .. } => "SUBREQUEST_MALFORMED_RESPONSE",
            FetchError::SubrequestUnexpectedPatchResponse { .. } => {
                "SUBREQUEST_UNEXPECTED_PATCH_RESPONSE"
            }
            FetchError::SubrequestHttpError { .. } => "SUBREQUEST_HTTP_ERROR",
            FetchError::SubrequestWsError { .. } => "SUBREQUEST_WEBSOCKET_ERROR",
            FetchError::ExecutionPathNotFound { .. } => "EXECUTION_PATH_NOT_FOUND",
            FetchError::MalformedRequest { .. } => "MALFORMED_REQUEST",
            FetchError::MalformedResponse { .. } => "MALFORMED_RESPONSE",
            FetchError::SubrequestBatchingError { .. } => "SUBREQUEST_BATCHING_ERROR",
        }
        .to_string()
    }
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
pub(crate) enum CacheResolverError {
    /// value retrieval failed: {0}
    RetrievalError(Arc<QueryPlannerError>),
    /// {0}
    Backpressure(crate::compute_job::ComputeBackPressureError),
    /// batch processing failed: {0}
    BatchingError(String),
}

impl IntoGraphQLErrors for CacheResolverError {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        match self {
            CacheResolverError::RetrievalError(retrieval_error) => retrieval_error
                .deref()
                .clone()
                .into_graphql_errors()
                .map_err(|_err| CacheResolverError::RetrievalError(retrieval_error)),
            CacheResolverError::Backpressure(e) => Ok(vec![e.to_graphql_error()]),
            CacheResolverError::BatchingError(msg) => Ok(vec![
                Error::builder()
                    .message(msg)
                    .extension_code("BATCH_PROCESSING_FAILED")
                    .build(),
            ]),
        }
    }
}

impl From<QueryPlannerError> for CacheResolverError {
    fn from(qp_err: QueryPlannerError) -> Self {
        Self::RetrievalError(Arc::new(qp_err))
    }
}

/// Error types for service building.
#[derive(Error, Debug, Display)]
pub(crate) enum ServiceBuildError {
    /// failed to initialize the query planner: {0}
    QpInitError(FederationError),

    /// schema error: {0}
    Schema(SchemaError),

    /// couldn't build Router service: {0}
    ServiceError(BoxError),
}

impl From<SchemaError> for ServiceBuildError {
    fn from(err: SchemaError) -> Self {
        ServiceBuildError::Schema(err)
    }
}

impl From<BoxError> for ServiceBuildError {
    fn from(err: BoxError) -> Self {
        ServiceBuildError::ServiceError(err)
    }
}

/// Error types for QueryPlanner
///
/// This error may be cached so no temporary errors may be defined here.
#[derive(Error, Debug, Display, Clone, Serialize, Deserialize)]
pub(crate) enum QueryPlannerError {
    /// invalid query: {0}
    OperationValidationErrors(ValidationErrors),

    /// query planning panicked: {0}
    JoinError(String),

    /// empty query plan. This behavior is unexpected and we suggest opening an issue to apollographql/router with a reproduction.
    EmptyPlan(String), // usage_reporting stats_report_key

    /// unhandled planner result
    UnhandledPlannerResult,

    /// spec error: {0}
    SpecError(SpecError),

    /// complexity limit exceeded
    LimitExceeded(OperationLimits<bool>),

    // Safe to cache because user scopes and policies are included in the cache key.
    /// Unauthorized field or type
    Unauthorized(Vec<Path>),

    /// Federation error: {0}
    FederationError(FederationErrorBridge),

    /// Query planning timed out: {0}
    Timeout(String),
}

impl From<FederationErrorBridge> for QueryPlannerError {
    fn from(value: FederationErrorBridge) -> Self {
        Self::FederationError(value)
    }
}

/// A temporary error type used to extract a few variants from `apollo-federation`'s
/// `FederationError`. For backwards compatibility, these other variant need to be extracted so
/// that the correct status code (GRAPHQL_VALIDATION_ERROR) can be added to the response. For
/// router 2.0, apollo-federation should split its error type into internal and external types.
/// When this happens, this temp type should be replaced with that type.
// TODO(@TylerBloom): See the comment above
#[derive(Error, Debug, Display, Clone, Serialize, Deserialize)]
pub(crate) enum FederationErrorBridge {
    /// {0}
    UnknownOperation(String),
    /// {0}
    OperationNameNotProvided(String),
    /// {0}
    Other(String),
    /// {0}
    Cancellation(String),
}

impl From<FederationError> for FederationErrorBridge {
    fn from(value: FederationError) -> Self {
        match &value {
            err @ FederationError::SingleFederationError(
                apollo_federation::error::SingleFederationError::UnknownOperation,
            ) => Self::UnknownOperation(err.to_string()),
            err @ FederationError::SingleFederationError(
                apollo_federation::error::SingleFederationError::OperationNameNotProvided,
            ) => Self::OperationNameNotProvided(err.to_string()),
            err @ FederationError::SingleFederationError(
                apollo_federation::error::SingleFederationError::PlanningCancelled,
            ) => Self::Cancellation(err.to_string()),
            err => Self::Other(err.to_string()),
        }
    }
}

impl IntoGraphQLErrors for FederationErrorBridge {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        match self {
            FederationErrorBridge::UnknownOperation(msg) => Ok(vec![
                Error::builder()
                    .message(msg)
                    .extension_code("GRAPHQL_VALIDATION_FAILED")
                    .build(),
            ]),
            FederationErrorBridge::OperationNameNotProvided(msg) => Ok(vec![
                Error::builder()
                    .message(msg)
                    .extension_code("GRAPHQL_VALIDATION_FAILED")
                    .build(),
            ]),
            // All other errors will be pushed on and be treated as internal server errors
            err => Err(err),
        }
    }
}

impl IntoGraphQLErrors for Vec<apollo_compiler::response::GraphQLError> {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        Ok(self
            .into_iter()
            .map(|err| {
                Error::builder()
                    .message(err.message)
                    .locations(
                        err.locations
                            .into_iter()
                            .map(|location| ErrorLocation {
                                line: location.line as u32,
                                column: location.column as u32,
                            })
                            .collect::<Vec<_>>(),
                    )
                    .extension_code("GRAPHQL_VALIDATION_FAILED")
                    .build()
            })
            .take(MAX_VALIDATION_ERRORS)
            .collect())
    }
}

impl IntoGraphQLErrors for QueryPlannerError {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        match self {
            QueryPlannerError::SpecError(err) => err
                .into_graphql_errors()
                .map_err(QueryPlannerError::SpecError),

            QueryPlannerError::OperationValidationErrors(errs) => errs
                .into_graphql_errors()
                .map_err(QueryPlannerError::OperationValidationErrors),

            QueryPlannerError::LimitExceeded(OperationLimits {
                depth,
                height,
                root_fields,
                aliases,
            }) => {
                let mut errors = Vec::new();
                let mut build = |exceeded, code, message| {
                    if exceeded {
                        errors.push(
                            Error::builder()
                                .message(message)
                                .extension_code(code)
                                .build(),
                        )
                    }
                };
                build(
                    depth,
                    "MAX_DEPTH_LIMIT",
                    "Maximum depth limit exceeded in this operation",
                );
                build(
                    height,
                    "MAX_HEIGHT_LIMIT",
                    "Maximum height (field count) limit exceeded in this operation",
                );
                build(
                    root_fields,
                    "MAX_ROOT_FIELDS_LIMIT",
                    "Maximum root fields limit exceeded in this operation",
                );
                build(
                    aliases,
                    "MAX_ALIASES_LIMIT",
                    "Maximum aliases limit exceeded in this operation",
                );
                Ok(errors)
            }
            QueryPlannerError::FederationError(err) => err
                .into_graphql_errors()
                .map_err(QueryPlannerError::FederationError),
            err => Err(err),
        }
    }
}

impl QueryPlannerError {
    pub(crate) fn usage_reporting(&self) -> Option<UsageReporting> {
        match self {
            QueryPlannerError::SpecError(e) => {
                Some(UsageReporting::Error(e.get_error_key().to_string()))
            }
            _ => None,
        }
    }
}

impl From<JoinError> for QueryPlannerError {
    fn from(err: JoinError) -> Self {
        QueryPlannerError::JoinError(err.to_string())
    }
}

impl From<SpecError> for QueryPlannerError {
    fn from(err: SpecError) -> Self {
        match err {
            SpecError::ValidationError(errors) => {
                QueryPlannerError::OperationValidationErrors(errors)
            }
            _ => QueryPlannerError::SpecError(err),
        }
    }
}

impl From<ValidationErrors> for QueryPlannerError {
    fn from(err: ValidationErrors) -> Self {
        QueryPlannerError::OperationValidationErrors(ValidationErrors { errors: err.errors })
    }
}
impl From<OperationLimits<bool>> for QueryPlannerError {
    fn from(error: OperationLimits<bool>) -> Self {
        QueryPlannerError::LimitExceeded(error)
    }
}

impl From<QueryPlannerError> for Response {
    fn from(err: QueryPlannerError) -> Self {
        FetchError::from(err).to_response()
    }
}

/// Error in the schema.
#[derive(Debug, Error, Display, derive_more::From)]
#[non_exhaustive]
pub(crate) enum SchemaError {
    /// URL parse error for subgraph {0}: {1}
    UrlParse(String, http::uri::InvalidUri),
    /// Could not find an URL for subgraph {0}
    #[from(ignore)]
    MissingSubgraphUrl(String),
    /// GraphQL parser error: {0}
    Parse(ParseErrors),
    /// GraphQL validation error: {0}
    Validate(ValidationErrors),
    /// Federation error: {0}
    FederationError(FederationError),
    /// Api error(s): {0}
    #[from(ignore)]
    Api(String),

    /// Connector error(s): {0}
    #[from(ignore)]
    Connector(FederationError),
}

/// Collection of schema validation errors.
#[derive(Debug)]
pub(crate) struct ParseErrors {
    pub(crate) errors: DiagnosticList,
}

impl std::fmt::Display for ParseErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut errors = self.errors.iter();
        for (i, error) in errors.by_ref().take(5).enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{error}")?;
        }
        let remaining = errors.count();
        if remaining > 0 {
            write!(f, "\n...and {remaining} other errors")?;
        }
        Ok(())
    }
}

impl IntoGraphQLErrors for ParseErrors {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        Ok(self
            .errors
            .iter()
            .map(|diagnostic| {
                Error::builder()
                    .message(diagnostic.error.to_string())
                    .locations(
                        diagnostic
                            .line_column_range()
                            .map(|location| {
                                vec![ErrorLocation {
                                    line: location.start.line as u32,
                                    column: location.start.column as u32,
                                }]
                            })
                            .unwrap_or_default(),
                    )
                    .extension_code("GRAPHQL_PARSING_FAILED")
                    .build()
            })
            .take(MAX_VALIDATION_ERRORS)
            .collect())
    }
}

/// Collection of schema validation errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ValidationErrors {
    pub(crate) errors: Vec<apollo_compiler::response::GraphQLError>,
}

impl ValidationErrors {
    pub(crate) fn into_graphql_errors_infallible(self) -> Vec<Error> {
        self.errors
            .iter()
            .map(|diagnostic| {
                Error::builder()
                    .message(diagnostic.message.to_string())
                    .locations(
                        diagnostic
                            .locations
                            .iter()
                            .map(|loc| ErrorLocation {
                                line: loc.line as u32,
                                column: loc.column as u32,
                            })
                            .collect(),
                    )
                    .extension_code("GRAPHQL_VALIDATION_FAILED")
                    .build()
            })
            .take(MAX_VALIDATION_ERRORS)
            .collect()
    }
}
impl IntoGraphQLErrors for ValidationErrors {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        Ok(self.into_graphql_errors_infallible())
    }
}

impl From<DiagnosticList> for ValidationErrors {
    fn from(errors: DiagnosticList) -> Self {
        Self {
            errors: errors
                .iter()
                .map(|e| e.unstable_to_json_compat())
                .take(MAX_VALIDATION_ERRORS)
                .collect(),
        }
    }
}

impl<T> From<WithErrors<T>> for ValidationErrors {
    fn from(WithErrors { errors, .. }: WithErrors<T>) -> Self {
        errors.into()
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            if let Some(location) = error.locations.first() {
                write!(
                    f,
                    "[{}:{}] {}",
                    location.line, location.column, error.message
                )?;
            } else {
                write!(f, "{}", error.message)?;
            }
        }
        Ok(())
    }
}

/// Error during subgraph batch processing
#[derive(Debug, Error, Display)]
pub(crate) enum SubgraphBatchingError {
    /// Sender unavailable
    SenderUnavailable,
    /// Requests is empty
    RequestsIsEmpty,
    /// Batch processing failed: {0}
    ProcessingFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assert_error_eq_ignoring_id;
    use crate::graphql;

    #[test]
    fn test_into_graphql_error() {
        let error = FetchError::SubrequestHttpError {
            status_code: Some(400),
            service: String::from("my_service"),
            reason: String::from("invalid request"),
        };
        let expected_gql_error = graphql::Error::builder()
            .message("HTTP fetch failed from 'my_service': invalid request")
            .extension_code("SUBREQUEST_HTTP_ERROR")
            .extension("reason", Value::String("invalid request".into()))
            .extension("service", Value::String("my_service".into()))
            .extension(
                "http",
                serde_json_bytes::json!({"status": Value::Number(400.into())}),
            )
            .build();

        assert_error_eq_ignoring_id!(expected_gql_error, error.to_graphql_error(None));
    }
}
