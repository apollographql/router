use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::validation::Valid;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::query_planner::{QueryPlanner, QueryPlannerConfig, QueryPlanOptions};
use apollo_federation::Supergraph;
use apollo_router_error::Error as RouterError;
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
            .map_err(|e| Error::PlanningFailed {
                message: e.to_string(),
            })
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
