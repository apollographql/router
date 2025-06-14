use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::validation::Valid;
use apollo_federation::Supergraph;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_router_error::Error as RouterError;
use serde::Deserialize;
use serde::Serialize;
use tower::Service;

use crate::Extensions;

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<Name>,
    pub document: Valid<ExecutableDocument>,
}

pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<Name>,
    pub query_plan: QueryPlan,
}

/// Serializable error detail for individual planning errors in GraphQL extensions
#[derive(
    Debug, Clone, Serialize, Deserialize, thiserror::Error, miette::Diagnostic, RouterError,
)]
pub enum PlanningErrorDetail {
    /// Operation name not found in the document
    #[error("Operation name not found")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_UNKNOWN_OPERATION),
        help("Ensure the operation name exists in your GraphQL document")
    )]
    UnknownOperation,

    /// Must provide operation name if query contains multiple operations
    #[error("Must provide operation name if query contains multiple operations")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_OPERATION_NAME_NOT_PROVIDED),
        help("Specify an operation name when your document contains multiple operations")
    )]
    OperationNameNotProvided,

    /// @defer is not supported on subscriptions
    #[error("@defer is not supported on subscriptions")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_DEFERRED_SUBSCRIPTION_UNSUPPORTED),
        help("Remove @defer directives from subscription operations")
    )]
    DeferredSubscriptionUnsupported,

    /// Query plan complexity exceeded: {message}
    #[error("Query plan complexity exceeded: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_COMPLEXITY_EXCEEDED),
        help("Simplify your GraphQL query to reduce planning complexity")
    )]
    QueryPlanComplexityExceeded {
        #[extension("complexityMessage")]
        message: String,
    },

    /// Query planning was cancelled
    #[error("Query planning was cancelled")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_PLANNING_CANCELLED),
        help("Check if the planning operation timed out or was interrupted")
    )]
    PlanningCancelled,

    /// No plan was found when subgraphs were disabled
    #[error("No plan was found when subgraphs were disabled")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_NO_PLAN_WITH_DISABLED_SUBGRAPHS),
        help(
            "Enable the necessary subgraphs or modify your query to work with available subgraphs"
        )
    )]
    NoPlanFoundWithDisabledSubgraphs,

    /// Invalid GraphQL: {message}
    #[error("Invalid GraphQL: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_INVALID_GRAPHQL),
        help("Check your GraphQL syntax and ensure it's valid")
    )]
    InvalidGraphQL {
        #[extension("graphqlMessage")]
        message: String,
    },

    /// Invalid subgraph: {message}
    #[error("Invalid subgraph: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_INVALID_SUBGRAPH),
        help("Check your subgraph schema configuration")
    )]
    InvalidSubgraph {
        #[extension("subgraphMessage")]
        message: String,
    },

    /// Other planning error: {message}
    #[error("Other planning error: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_OTHER_PLANNING_ERROR),
        help("Check the error message for specific details about this planning issue")
    )]
    Other {
        #[extension("errorMessage")]
        message: String,
    },
}

impl From<apollo_federation::error::SingleFederationError> for PlanningErrorDetail {
    fn from(error: apollo_federation::error::SingleFederationError) -> Self {
        use apollo_federation::error::SingleFederationError;

        match error {
            SingleFederationError::UnknownOperation => Self::UnknownOperation,
            SingleFederationError::OperationNameNotProvided => Self::OperationNameNotProvided,
            SingleFederationError::DeferredSubscriptionUnsupported => {
                Self::DeferredSubscriptionUnsupported
            }
            SingleFederationError::QueryPlanComplexityExceeded { message } => {
                Self::QueryPlanComplexityExceeded { message }
            }
            SingleFederationError::PlanningCancelled => Self::PlanningCancelled,
            SingleFederationError::NoPlanFoundWithDisabledSubgraphs => {
                Self::NoPlanFoundWithDisabledSubgraphs
            }
            SingleFederationError::InvalidGraphQL { message } => Self::InvalidGraphQL { message },
            SingleFederationError::InvalidSubgraph { message } => Self::InvalidSubgraph { message },
            other => Self::Other {
                message: other.to_string(),
            },
        }
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic, RouterError)]
pub enum Error {
    /// Query planning failed: {message}
    #[error("Query planning failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_PLANNING_FAILED),
        help("Check your GraphQL query syntax and schema compatibility")
    )]
    PlanningFailed {
        #[extension("planningMessage")]
        message: String,
    },

    /// Multiple query planning errors occurred
    #[error("Multiple query planning errors occurred: {count} errors")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_MULTIPLE_PLANNING_ERRORS),
        help("Review all planning errors and fix the underlying issues")
    )]
    MultiplePlanningErrors {
        #[extension("errorCount")]
        count: usize,
        #[extension("planningErrors")]
        errors: Vec<PlanningErrorDetail>,
    },

    /// Federation error occurred: {message}
    #[error("Federation error: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_FEDERATION_ERROR),
        help("Check your supergraph schema configuration")
    )]
    FederationError {
        #[extension("federationMessage")]
        message: String,
    },

    /// Invalid supergraph configuration: {reason}
    #[error("Invalid supergraph configuration: {reason}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PLAN_INVALID_SUPERGRAPH),
        help("Verify your supergraph schema is properly configured")
    )]
    InvalidSupergraph {
        #[extension("configReason")]
        reason: String,
    },
}

impl From<apollo_federation::error::FederationError> for Error {
    fn from(federation_error: apollo_federation::error::FederationError) -> Self {
        // Extract all errors using the public method
        let all_errors = federation_error.into_errors();

        if all_errors.len() == 1 {
            // Single error case
            let single_error = &all_errors[0];
            Self::PlanningFailed {
                message: single_error.to_string(),
            }
        } else {
            // Multiple errors case
            let errors: Vec<PlanningErrorDetail> = all_errors.into_iter().map(Into::into).collect();

            Self::MultiplePlanningErrors {
                count: errors.len(),
                errors,
            }
        }
    }
}

/// Query planning service that transforms validated ExecutableDocuments into QueryPlans
///
/// This service uses apollo-federation's QueryPlanner to generate query plans from
/// validated GraphQL documents against a federated supergraph schema.
#[derive(Clone)]
pub struct QueryPlanService {
    query_planner: Arc<QueryPlanner>,
}

impl QueryPlanService {
    /// Create a new QueryPlanService with the given supergraph and configuration
    pub fn new(supergraph: Supergraph, config: QueryPlannerConfig) -> Result<Self, Error> {
        let query_planner =
            QueryPlanner::new(&supergraph, config).map_err(|e| Error::FederationError {
                message: e.to_string(),
            })?;

        Ok(Self {
            query_planner: Arc::new(query_planner),
        })
    }

    /// Create a new QueryPlanService with default configuration
    pub fn with_supergraph(supergraph: Supergraph) -> Result<Self, Error> {
        Self::new(supergraph, QueryPlannerConfig::default())
    }

    /// Generate a query plan for the given document and operation
    async fn plan_query(
        &self,
        document: &Valid<ExecutableDocument>,
        operation_name: Option<Name>,
    ) -> Result<QueryPlan, Error> {
        let options = QueryPlanOptions::default();

        self.query_planner
            .build_query_plan(document, operation_name, options)
            .map_err(Into::into)
    }
}

impl Service<Request> for QueryPlanService {
    type Response = Response;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let document = req.document;
        let operation_name = req.operation_name;
        let extensions = req.extensions;
        let service = self.clone();

        Box::pin(async move {
            // Generate the query plan using apollo-federation
            let query_plan = service
                .plan_query(&document, operation_name.clone())
                .await?;

            Ok(Response {
                extensions,
                operation_name,
                query_plan,
            })
        })
    }
}

#[cfg(test)]
mod tests;
