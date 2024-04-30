//! Router errors.
use std::sync::Arc;

use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::validation::WithErrors;
use apollo_federation::error::FederationError;
use displaydoc::Display;
use lazy_static::__Deref;
use router_bridge::introspect::IntrospectionError;
use router_bridge::planner::PlannerError;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tokio::task::JoinError;
use tower::BoxError;

pub(crate) use crate::configuration::ConfigurationError;
pub(crate) use crate::graphql::Error;
use crate::graphql::ErrorExtension;
use crate::graphql::IntoGraphQLErrors;
use crate::graphql::Location as ErrorLocation;
use crate::graphql::Response;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::spec::operation_limits::OperationLimits;
use crate::spec::SpecError;

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
                FetchError::ValidationInvalidTypeVariable { name } => {
                    extensions
                        .entry("name")
                        .or_insert_with(|| name.clone().into());
                }
                _ => (),
            }
        }

        Error {
            message: self.to_string(),
            locations: Default::default(),
            path,
            extensions: value.as_object().unwrap().to_owned(),
        }
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
#[derive(Error, Debug, Display, Clone, Serialize, Deserialize)]
pub(crate) enum CacheResolverError {
    /// value retrieval failed: {0}
    RetrievalError(Arc<QueryPlannerError>),
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
            CacheResolverError::BatchingError(msg) => Ok(vec![Error::builder()
                .message(msg)
                .extension_code("BATCH_PROCESSING_FAILED")
                .build()]),
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
    /// couldn't build Query Planner Service: {0}
    QueryPlannerError(QueryPlannerError),

    /// The supergraph schema failed to produce a valid API schema: {0}
    ApiSchemaError(FederationError),

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

impl From<FederationError> for ServiceBuildError {
    fn from(err: FederationError) -> Self {
        ServiceBuildError::ApiSchemaError(err)
    }
}

impl From<Vec<PlannerError>> for ServiceBuildError {
    fn from(errors: Vec<PlannerError>) -> Self {
        ServiceBuildError::QueryPlannerError(errors.into())
    }
}

impl From<router_bridge::error::Error> for ServiceBuildError {
    fn from(error: router_bridge::error::Error) -> Self {
        ServiceBuildError::QueryPlannerError(error.into())
    }
}

impl From<BoxError> for ServiceBuildError {
    fn from(err: BoxError) -> Self {
        ServiceBuildError::ServiceError(err)
    }
}

/// Error types for QueryPlanner
#[derive(Error, Debug, Display, Clone, Serialize, Deserialize)]
pub(crate) enum QueryPlannerError {
    /// couldn't instantiate query planner; invalid schema: {0}
    SchemaValidationErrors(PlannerErrors),

    /// invalid query: {0}
    OperationValidationErrors(ValidationErrors),

    /// couldn't plan query: {0}
    PlanningErrors(PlanErrors),

    /// query planning panicked: {0}
    JoinError(String),

    /// Cache resolution failed: {0}
    CacheResolverError(Arc<CacheResolverError>),

    /// empty query plan. This behavior is unexpected and we suggest opening an issue to apollographql/router with a reproduction.
    EmptyPlan(UsageReporting), // usage_reporting_signature

    /// unhandled planner result
    UnhandledPlannerResult,

    /// router bridge error: {0}
    RouterBridgeError(router_bridge::error::Error),

    /// spec error: {0}
    SpecError(SpecError),

    /// introspection error: {0}
    Introspection(IntrospectionError),

    /// complexity limit exceeded
    LimitExceeded(OperationLimits<bool>),

    /// Unauthorized field or type
    Unauthorized(Vec<Path>),

    /// Query planner pool error: {0}
    PoolProcessing(String),

    /// Federation error: {0}
    // TODO: make `FederationError` serializable and store it as-is?
    FederationError(String),
}

impl IntoGraphQLErrors for Vec<apollo_compiler::execution::GraphQLError> {
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
            .collect())
    }
}

impl IntoGraphQLErrors for QueryPlannerError {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        match self {
            QueryPlannerError::SpecError(err) => err
                .into_graphql_errors()
                .map_err(QueryPlannerError::SpecError),
            QueryPlannerError::SchemaValidationErrors(errs) => errs
                .into_graphql_errors()
                .map_err(QueryPlannerError::SchemaValidationErrors),
            QueryPlannerError::OperationValidationErrors(errs) => errs
                .into_graphql_errors()
                .map_err(QueryPlannerError::OperationValidationErrors),
            QueryPlannerError::PlanningErrors(planning_errors) => Ok(planning_errors
                .errors
                .iter()
                .map(|p_err| Error::from(p_err.clone()))
                .collect()),
            QueryPlannerError::Introspection(introspection_error) => Ok(vec![Error::builder()
                .message(
                    introspection_error
                        .message
                        .unwrap_or_else(|| "introspection error".to_string()),
                )
                .extension_code("INTROSPECTION_ERROR")
                .build()]),
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
            err => Err(err),
        }
    }
}

#[derive(Clone, Debug, Error, Serialize, Deserialize)]
/// Container for planner setup errors
pub(crate) struct PlannerErrors(Arc<Vec<PlannerError>>);

impl IntoGraphQLErrors for PlannerErrors {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        let errors = self.0.iter().map(|e| Error::from(e.clone())).collect();

        Ok(errors)
    }
}

impl std::fmt::Display for PlannerErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "schema validation errors: {}",
            self.0
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<String>>()
                .join(", ")
        ))
    }
}

impl From<Vec<PlannerError>> for QueryPlannerError {
    fn from(errors: Vec<PlannerError>) -> Self {
        QueryPlannerError::SchemaValidationErrors(PlannerErrors(Arc::new(errors)))
    }
}

impl From<router_bridge::planner::PlanErrors> for QueryPlannerError {
    fn from(errors: router_bridge::planner::PlanErrors) -> Self {
        QueryPlannerError::PlanningErrors(errors.into())
    }
}

impl From<PlanErrors> for QueryPlannerError {
    fn from(errors: PlanErrors) -> Self {
        QueryPlannerError::PlanningErrors(errors)
    }
}

impl From<JoinError> for QueryPlannerError {
    fn from(err: JoinError) -> Self {
        QueryPlannerError::JoinError(err.to_string())
    }
}

impl From<CacheResolverError> for QueryPlannerError {
    fn from(err: CacheResolverError) -> Self {
        QueryPlannerError::CacheResolverError(Arc::new(err))
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

impl From<router_bridge::error::Error> for QueryPlannerError {
    fn from(error: router_bridge::error::Error) -> Self {
        QueryPlannerError::RouterBridgeError(error)
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

/// The payload if the plan_worker invocation failed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PlanErrors {
    /// The errors the plan_worker invocation failed with
    pub(crate) errors: Arc<Vec<router_bridge::planner::PlanError>>,
    /// Usage reporting related data such as the
    /// operation signature and referenced fields
    pub(crate) usage_reporting: UsageReporting,
}

impl From<router_bridge::planner::PlanErrors> for PlanErrors {
    fn from(
        router_bridge::planner::PlanErrors {
            errors,
            usage_reporting,
        }: router_bridge::planner::PlanErrors,
    ) -> Self {
        PlanErrors {
            errors,
            usage_reporting,
        }
    }
}

impl std::fmt::Display for PlanErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "query validation errors: {}",
            self.errors
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
    FederationError(apollo_federation::error::FederationError),
    /// Api error(s): {0}
    #[from(ignore)]
    Api(String),
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
            write!(f, "{}", error)?;
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
                            .get_line_column()
                            .map(|location| {
                                vec![ErrorLocation {
                                    line: location.line as u32,
                                    column: location.column as u32,
                                }]
                            })
                            .unwrap_or_default(),
                    )
                    .extension_code("GRAPHQL_PARSING_FAILED")
                    .build()
            })
            .collect())
    }
}

/// Collection of schema validation errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ValidationErrors {
    pub(crate) errors: Vec<apollo_compiler::execution::GraphQLError>,
}

impl IntoGraphQLErrors for ValidationErrors {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        Ok(self
            .errors
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
            .collect())
    }
}

impl From<DiagnosticList> for ValidationErrors {
    fn from(errors: DiagnosticList) -> Self {
        Self {
            errors: errors.iter().map(|e| e.unstable_to_json_compat()).collect(),
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
    /// Request does not have a subgraph name
    MissingSubgraphName,
    /// Requests is empty
    RequestsIsEmpty,
    /// Batch processing failed: {0}
    ProcessingFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
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

        assert_eq!(expected_gql_error, error.to_graphql_error(None));
    }

    #[test]
    fn test_into_graphql_error_introspection_with_message_handled_correctly() {
        let expected_message = "no can introspect".to_string();
        let ie = IntrospectionError {
            message: Some(expected_message.clone()),
        };
        let error = QueryPlannerError::Introspection(ie);
        let mut graphql_errors = error.into_graphql_errors().expect("vec of graphql errors");
        assert_eq!(graphql_errors.len(), 1);
        let first_error = graphql_errors.pop().expect("has to be one error");
        assert_eq!(first_error.message, expected_message);
        assert_eq!(first_error.extensions.len(), 1);
        assert_eq!(
            first_error.extensions.get("code").expect("has code"),
            "INTROSPECTION_ERROR"
        );
    }

    #[test]
    fn test_into_graphql_error_introspection_without_message_handled_correctly() {
        let expected_message = "introspection error".to_string();
        let ie = IntrospectionError { message: None };
        let error = QueryPlannerError::Introspection(ie);
        let mut graphql_errors = error.into_graphql_errors().expect("vec of graphql errors");
        assert_eq!(graphql_errors.len(), 1);
        let first_error = graphql_errors.pop().expect("has to be one error");
        assert_eq!(first_error.message, expected_message);
        assert_eq!(first_error.extensions.len(), 1);
        assert_eq!(
            first_error.extensions.get("code").expect("has code"),
            "INTROSPECTION_ERROR"
        );
    }
}
