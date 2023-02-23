//! Router errors.
use std::sync::Arc;

use displaydoc::Display;
use lazy_static::__Deref;
use miette::Diagnostic;
use miette::NamedSource;
use miette::Report;
use miette::SourceSpan;
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
use crate::spec::SpecError;

/// Error types for execution.
///
/// Note that these are not actually returned to the client, but are instead converted to JSON for
/// [`struct@Error`].
#[derive(Error, Display, Debug, Clone, Serialize)]
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
        let mut value: Value = serde_json::to_value(self).unwrap_or_default().into();
        if let Some(extensions) = value.as_object_mut() {
            extensions
                .entry("code")
                .or_insert_with(|| self.extension_code().into());
            // Following these specs https://www.apollographql.com/docs/apollo-server/data/errors/#including-custom-error-details
            match self {
                FetchError::SubrequestMalformedResponse { service, .. }
                | FetchError::SubrequestUnexpectedPatchResponse { service }
                | FetchError::SubrequestHttpError { service, .. }
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
            FetchError::ExecutionFieldNotFound { .. } => "EXECUTION_FIELD_NOT_FOUND",
            FetchError::ExecutionPathNotFound { .. } => "EXECUTION_PATH_NOT_FOUND",
            FetchError::CompressionError { .. } => "COMPRESSION_ERROR",
            #[cfg(test)]
            FetchError::ExecutionInvalidContent { .. } => "EXECUTION_INVALID_CONTENT",
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
#[derive(Error, Debug, Display, Clone)]
pub(crate) enum ServiceBuildError {
    /// couldn't build Router Service: {0}
    QueryPlannerError(QueryPlannerError),
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
            err => Err(err),
        }
    }
}

impl ErrorExtension for QueryPlannerError {
    fn extension_code(&self) -> String {
        match self {
            QueryPlannerError::SchemaValidationErrors(_) => "SCHEMA_VALIDATION_ERRORS",
            QueryPlannerError::PlanningErrors(_) => "PLANNING_ERRORS",
            QueryPlannerError::JoinError(_) => "JOIN_ERROR",
            QueryPlannerError::CacheResolverError(_) => "CACHE_RESOLVER_ERROR",
            QueryPlannerError::EmptyPlan(_) => "EMPTY_PLAN",
            QueryPlannerError::UnhandledPlannerResult => "UNHANDLED_PLANNER_RESULT",
            QueryPlannerError::RouterBridgeError(_) => "ROUTER_BRIDGE_ERROR",
            QueryPlannerError::SpecError(_) => "SPEC_ERROR",
            QueryPlannerError::Introspection(_) => "INTROSPECTION",
        }
        .to_string()
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
    /// Parsing error(s).
    Parse(ParseErrors),
    /// Api error(s): {0}
    Api(String),
}

/// Collection of schema parsing errors.
#[derive(Debug)]
pub(crate) struct ParseErrors {
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
    pub(crate) fn print(&self) {
        if LevelFilter::current() == LevelFilter::OFF && cfg!(not(debug_assertions)) {
            return;
        } else if atty::is(atty::Stream::Stdout) {
            // Fancy Miette reports for TTYs
            self.errors.iter().for_each(|err| {
                let report = Report::new(ParserError {
                    src: NamedSource::new("supergraph_schema", self.raw_schema.clone()),
                    span: (err.index(), err.data().len()).into(),
                    ty: err.message().into(),
                });
                // `format!` works around https://github.com/rust-lang/rust/issues/107118
                // to test the panic from https://github.com/apollographql/router/issues/2269
                #[allow(clippy::format_in_format_args)]
                {
                    println!("{}", format!("{report:?}"));
                }
            });
        } else {
            // Best effort to display errors
            self.errors.iter().for_each(|r| {
                println!("{r:#?}");
            });
        };
    }
}

/// Error types for licensing.
#[derive(Error, Display, Debug, Clone, Serialize, Deserialize)]
pub(crate) enum LicenseError {
    /// Apollo graph reference is missing
    MissingGraphReference,
    /// Apollo key is missing
    MissingKey,
}
