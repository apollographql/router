use crate::Extensions;
use crate::json::JsonValue;
use crate::services::query_parse;
use crate::services::query_plan;
use apollo_compiler::Name;
use apollo_federation::query_plan::QueryPlan;
use apollo_router_error::Error as RouterError;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::{BoxError, Service, ServiceExt};

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub body: JsonValue,
}

#[derive(Debug)]
pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query_plan: QueryPlan,
    pub query_variables: HashMap<String, Value>,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic, RouterError)]
pub enum Error {
    /// Query parsing failed during preparation: {message}
    #[error("Query parsing failed during preparation: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PREPARATION_PARSING_FAILED),
        help("Check your GraphQL query syntax and schema compatibility")
    )]
    ParsingFailed {
        #[extension("parsingMessage")]
        message: String,
    },

    /// Query planning failed during preparation: {message}
    #[error("Query planning failed during preparation: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PREPARATION_PLANNING_FAILED),
        help("Check your GraphQL query and schema compatibility")
    )]
    PlanningFailed {
        #[extension("planningMessage")]
        message: String,
    },

    /// JSON extraction failed: {field}
    #[error("JSON extraction failed: {field}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PREPARATION_JSON_EXTRACTION_FAILED),
        help("Ensure the JSON request contains the required GraphQL fields")
    )]
    JsonExtraction {
        #[extension("jsonField")]
        field: String,
    },

    /// Variable extraction failed
    #[error("Variable extraction failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PREPARATION_VARIABLE_EXTRACTION_FAILED),
        help("Check that your GraphQL variables are properly formatted JSON")
    )]
    VariableExtraction,
}

impl From<query_parse::Error> for Error {
    fn from(error: query_parse::Error) -> Self {
        Self::ParsingFailed {
            message: error.to_string(),
        }
    }
}

impl From<query_plan::Error> for Error {
    fn from(error: query_plan::Error) -> Self {
        Self::PlanningFailed {
            message: error.to_string(),
        }
    }
}

#[cfg_attr(test, mry::mry)]
pub trait QueryPreparation {
    /// Transforms a JSON request containing a GraphQL query into an execution request
    /// containing a fully prepared query plan ready for execution.
    ///
    /// This service handles the complete query preparation pipeline:
    /// 1. Extract GraphQL query string from JSON request
    /// 2. Parse and validate the query against the schema  
    /// 3. Generate an optimized query plan
    /// 4. Transform the result into an execution request
    ///
    /// # Errors
    ///
    /// Returns `Error::ParsingFailed` if the GraphQL query cannot be parsed or validated.
    /// Returns `Error::PlanningFailed` if query planning fails.
    /// Returns `Error::JsonExtraction` if required fields cannot be extracted from the JSON request.
    fn call(&self, req: Request) -> impl std::future::Future<Output = Result<Response, Error>> + Send;
}

/// Query preparation service that combines query parsing and planning into a single operation
///
/// This composite service orchestrates the query_parse and query_plan services to transform
/// JSON requests containing GraphQL queries into execution requests with query plans.
#[derive(Clone)]
pub struct QueryPreparationService<ParseService, PlanService> {
    parse_service: ParseService,
    plan_service: PlanService,
}

impl<ParseService, PlanService> QueryPreparationService<ParseService, PlanService>
where
    ParseService:
        Service<query_parse::Request, Response = query_parse::Response> + Clone + Send + 'static,
    ParseService::Error: Into<BoxError>,
    ParseService::Future: Send,
    PlanService:
        Service<query_plan::Request, Response = query_plan::Response> + Clone + Send + 'static,
    PlanService::Error: Into<BoxError>,
    PlanService::Future: Send,
{
    pub fn new(parse_service: ParseService, plan_service: PlanService) -> Self {
        Self {
            parse_service,
            plan_service,
        }
    }
}

impl<ParseService, PlanService> Service<Request>
    for QueryPreparationService<ParseService, PlanService>
where
    ParseService:
        Service<query_parse::Request, Response = query_parse::Response> + Clone + Send + 'static,
    ParseService::Error: Into<BoxError>,
    ParseService::Future: Send,
    PlanService:
        Service<query_plan::Request, Response = query_plan::Response> + Clone + Send + 'static,
    PlanService::Error: Into<BoxError>,
    PlanService::Future: Send,
{
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Check that both services are ready
        match self.parse_service.poll_ready(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
            Poll::Ready(Ok(())) => match self.plan_service.poll_ready(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
                Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            },
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let mut parse_service = self.parse_service.clone();
        let mut plan_service = self.plan_service.clone();

        Box::pin(async move {
            // 1. Extract JSON request data
            let (query_string, operation_name, variables) =
                extract_graphql_request(&req.body).map_err(|e| Box::new(e) as BoxError)?;

            // 2. Create extended extensions for inner services (following hexagonal architecture)
            let extended_extensions = req.extensions.extend();

            // 3. Transform JSON to QueryParse request
            let parse_req = query_parse::Request {
                extensions: extended_extensions.clone(),
                operation_name: operation_name.clone(),
                query: query_string,
            };

            // 4. Call query_parse service - if parsing fails, don't proceed to planning
            let parse_resp = parse_service
                .ready()
                .await
                .map_err(Into::into)?
                .call(parse_req)
                .await
                .map_err(Into::into)?;

            // 5. Convert operation_name from String to Name for query planning
            let operation_name_for_planning = operation_name
                .as_ref()
                .and_then(|name| Name::try_from(name.as_str()).ok());

            // 6. Transform QueryParse response to QueryPlan request
            let plan_req = query_plan::Request {
                extensions: extended_extensions,
                operation_name: operation_name_for_planning,
                document: parse_resp.query,
            };

            // 7. Call query_plan service - if planning fails, don't proceed
            let plan_resp = plan_service
                .ready()
                .await
                .map_err(Into::into)?
                .call(plan_req)
                .await
                .map_err(Into::into)?;

            // 8. Transform QueryPlan response to final Response (returning original extensions)
            Ok(Response {
                extensions: req.extensions, // Return original extensions as per hexagonal architecture
                operation_name,
                query_plan: plan_resp.query_plan,
                query_variables: variables,
            })
        })
    }
}

/// Extract GraphQL request components from JSON body
///
/// Expected JSON format:
/// ```json
/// {
///   "query": "query GetUser($id: ID!) { user(id: $id) { name } }",
///   "operationName": "GetUser",
///   "variables": { "id": "123" }
/// }
/// ```
fn extract_graphql_request(
    body: &JsonValue,
) -> Result<(String, Option<String>, HashMap<String, Value>), Error> {
    // Extract query string (required)
    let query = body
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::JsonExtraction {
            field: "query".to_string(),
        })?;

    // Extract operation name (optional)
    let operation_name = body
        .get("operationName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Extract variables (optional, defaults to empty map)
    let variables = match body.get("variables") {
        Some(vars) => {
            if vars.is_null() {
                HashMap::new()
            } else {
                vars.as_object()
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .ok_or_else(|| Error::VariableExtraction)?
            }
        }
        None => HashMap::new(),
    };

    Ok((query, operation_name, variables))
}

impl QueryPreparation
    for QueryPreparationService<query_parse::QueryParseService, query_plan::QueryPlanService>
{
    fn call(&self, req: Request) -> impl std::future::Future<Output = Result<Response, Error>> + Send {
        use tower::ServiceExt;
        let service = self.clone();
        async move {
            service.oneshot(req).await.map_err(|boxed_error| {
                // Try to downcast the BoxError back to our specific Error type
                match boxed_error.downcast::<Error>() {
                    Ok(specific_error) => *specific_error,
                    Err(other_error) => {
                        // If it's not our specific error type, create a generic error
                        Error::ParsingFailed {
                            message: other_error.to_string(),
                        }
                    }
                }
            })
        }
    }
}

#[cfg(test)]
mod tests;
