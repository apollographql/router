use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::validation::Valid;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::query_planner::{QueryPlanner, QueryPlannerConfig, QueryPlanOptions};
use apollo_federation::Supergraph;
use apollo_router_error::Error as RouterError;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::Service;

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
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error, miette::Diagnostic)]
#[error("Planning error: {message}")]
#[diagnostic(
    code(APOLLO_ROUTER_QUERY_PLAN_PLANNING_ERROR_DETAIL),
    help("Check the specific error message for details about this planning issue")
)]
pub struct PlanningErrorDetail {
    /// Error message describing the planning failure
    pub message: String,
    /// Optional error code for categorizing the error type
    pub code: Option<String>,
}

impl PlanningErrorDetail {
    /// Create a new planning error detail from a federation error
    pub fn from_federation_error(error: apollo_federation::error::SingleFederationError) -> Self {
        Self {
            message: error.to_string(),
            code: Self::extract_federation_error_code(&error),
        }
    }

    /// Extract error code from SingleFederationError for categorization
    pub fn extract_federation_error_code(error: &apollo_federation::error::SingleFederationError) -> Option<String> {
        use apollo_federation::error::SingleFederationError;
        
        match error {
            SingleFederationError::UnknownOperation => Some("UNKNOWN_OPERATION".to_string()),
            SingleFederationError::OperationNameNotProvided => Some("OPERATION_NAME_NOT_PROVIDED".to_string()),
            SingleFederationError::DeferredSubscriptionUnsupported => Some("DEFERRED_SUBSCRIPTION_UNSUPPORTED".to_string()),
            SingleFederationError::QueryPlanComplexityExceeded { .. } => Some("QUERY_PLAN_COMPLEXITY_EXCEEDED".to_string()),
            SingleFederationError::PlanningCancelled => Some("PLANNING_CANCELLED".to_string()),
            SingleFederationError::NoPlanFoundWithDisabledSubgraphs => Some("NO_PLAN_FOUND_WITH_DISABLED_SUBGRAPHS".to_string()),
            SingleFederationError::InvalidGraphQL { .. } => Some("INVALID_GRAPHQL".to_string()),
            SingleFederationError::InvalidSubgraph { .. } => Some("INVALID_SUBGRAPH".to_string()),
            _ => None, // For other error types, don't provide a specific code
        }
    }
}

impl apollo_router_error::Error for PlanningErrorDetail {
    fn error_code(&self) -> &'static str {
        "APOLLO_ROUTER_QUERY_PLAN_PLANNING_ERROR_DETAIL"
    }

    fn populate_graphql_extensions(&self, extensions_map: &mut std::collections::BTreeMap<String, serde_json::Value>) {
        // Add the message and code fields to GraphQL extensions
        extensions_map.insert(
            "message".to_string(),
            serde_json::Value::String(self.message.clone()),
        );
        
        if let Some(ref code) = self.code {
            extensions_map.insert(
                "code".to_string(),
                serde_json::Value::String(code.clone()),
            );
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
            let errors: Vec<PlanningErrorDetail> = all_errors
                .into_iter()
                .map(PlanningErrorDetail::from_federation_error)
                .collect();
            
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
        let query_planner = QueryPlanner::new(&supergraph, config)
            .map_err(|e| Error::FederationError {
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
            let query_plan = service.plan_query(&document, operation_name.clone()).await?;

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
