//! Router errors.
use std::sync::Arc;

use displaydoc::Display;
use lazy_static::__Deref;
use router_bridge::introspect::IntrospectionError;
use router_bridge::planner::PlannerError;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tokio::task::JoinError;
use tracing::level_filters::LevelFilter;

pub(crate) use crate::configuration::ConfigurationError;
pub(crate) use crate::graphql::Error;
use crate::graphql::ErrorExtension;
use crate::graphql::IntoGraphQLErrors;
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

    /// subquery requires field '{field}' but it was not found in the current response
    ExecutionFieldNotFound {
        /// The field that is not found.
        field: String,
    },

    #[cfg(test)]
    /// invalid content: {reason}
    ExecutionInvalidContent { reason: String },

    /// could not find path: {reason}
    ExecutionPathNotFound { reason: String },
    /// could not compress request: {reason}
    CompressionError {
        /// The service that failed.
        service: String,
        /// The reason the compression failed.
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
                | FetchError::SubrequestWsError { service, .. }
                | FetchError::CompressionError { service, .. } => {
                    extensions
                        .entry("service")
                        .or_insert_with(|| service.clone().into());
                }
                FetchError::ExecutionFieldNotFound { field, .. } => {
                    extensions
                        .entry("field")
                        .or_insert_with(|| field.clone().into());
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
            FetchError::ExecutionFieldNotFound { .. } => "EXECUTION_FIELD_NOT_FOUND",
            FetchError::ExecutionPathNotFound { .. } => "EXECUTION_PATH_NOT_FOUND",
            FetchError::CompressionError { .. } => "COMPRESSION_ERROR",
            #[cfg(test)]
            FetchError::ExecutionInvalidContent { .. } => "EXECUTION_INVALID_CONTENT",
            FetchError::MalformedRequest { .. } => "MALFORMED_REQUEST",
            FetchError::MalformedResponse { .. } => "MALFORMED_RESPONSE",
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
}

impl IntoGraphQLErrors for CacheResolverError {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        let CacheResolverError::RetrievalError(retrieval_error) = self;
        retrieval_error
            .deref()
            .clone()
            .into_graphql_errors()
            .map_err(|_err| CacheResolverError::RetrievalError(retrieval_error))
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
    /// couldn't build Router Service: {0}
    QueryPlannerError(QueryPlannerError),

    /// schema error: {0}
    Schema(SchemaError),
}

impl From<SchemaError> for ServiceBuildError {
    fn from(err: SchemaError) -> Self {
        ServiceBuildError::Schema(err)
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

/// Error types for QueryPlanner
#[derive(Error, Debug, Display, Clone, Serialize, Deserialize)]
pub(crate) enum QueryPlannerError {
    /// couldn't instantiate query planner; invalid schema: {0}
    SchemaValidationErrors(PlannerErrors),

    /// couldn't plan query: {0}
    PlanningErrors(PlanErrors),

    /// query planning panicked: {0}
    JoinError(String),

    /// Cache resolution failed: {0}
    CacheResolverError(Arc<CacheResolverError>),

    /// empty query plan. This often means an unhandled Introspection query was sent. Please file an issue to apollographql/router.
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
}

impl IntoGraphQLErrors for QueryPlannerError {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        match self {
            QueryPlannerError::SpecError(err) => {
                let gql_err = match err.custom_extension_details() {
                    Some(extension_details) => Error::builder()
                        .message(err.to_string())
                        .extension_code(err.extension_code())
                        .extensions(extension_details)
                        .build(),
                    None => Error::builder()
                        .message(err.to_string())
                        .extension_code(err.extension_code())
                        .build(),
                };

                Ok(vec![gql_err])
            }
            QueryPlannerError::SchemaValidationErrors(errs) => errs
                .into_graphql_errors()
                .map_err(QueryPlannerError::SchemaValidationErrors),
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
        QueryPlannerError::SpecError(err)
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
#[derive(Debug, Error, Display)]
#[non_exhaustive]
pub(crate) enum SchemaError {
    /// URL parse error for subgraph {0}: {1}
    UrlParse(String, http::uri::InvalidUri),
    /// Could not find an URL for subgraph {0}
    MissingSubgraphUrl(String),
    /// GraphQL parser error(s).
    Parse(ParseErrors),
    /// GraphQL parser or validation error(s).
    Validate(ValidationErrors),
    /// Api error(s): {0}
    Api(String),
}

/// Collection of schema validation errors.
#[derive(Clone, Debug)]
pub(crate) struct ParseErrors {
    pub(crate) errors: Vec<apollo_parser::Error>,
}

impl std::fmt::Display for ParseErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut errors = self.errors.iter();
        if let Some(error) = errors.next() {
            write!(f, "{}", error.message())?;
        }
        for error in errors {
            write!(f, "\n{}", error.message())?;
        }
        Ok(())
    }
}

/// Collection of schema validation errors.
#[derive(Clone, Debug)]
pub(crate) struct ValidationErrors {
    pub(crate) errors: Vec<apollo_compiler::ApolloDiagnostic>,
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut errors = self.errors.iter();
        if let Some(error) = errors.next() {
            write!(f, "{}", error.data)?;
        }
        for error in errors {
            write!(f, "\n{}", error.data)?;
        }
        Ok(())
    }
}

impl ValidationErrors {
    #[allow(clippy::needless_return)]
    pub(crate) fn print(&self) {
        if LevelFilter::current() == LevelFilter::OFF && cfg!(not(debug_assertions)) {
            return;
        } else if atty::is(atty::Stream::Stdout) {
            // Fancy reports for TTYs
            self.errors.iter().for_each(|err| {
                // `format!` works around https://github.com/rust-lang/rust/issues/107118
                // to test the panic from https://github.com/apollographql/router/issues/2269
                #[allow(clippy::format_in_format_args)]
                {
                    println!("{}", format!("{err}"));
                }
            });
        } else {
            // Best effort to display errors
            self.errors.iter().for_each(|diag| {
                println!("{}", diag.data);
            });
        };
    }
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
