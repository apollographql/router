use crate::services::json_server::{Request as JsonRequest, Response as JsonResponse};
use crate::services::query_execution::{Request as ExecutionRequest, Response as ExecutionResponse};
use crate::services::query_parse;
use crate::services::query_plan::{self, QueryPlanning};
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, Error)]
pub enum Error {
    /// Query parsing failed: {0}
    #[error("Query parsing failed: {0}")]
    QueryParse(#[from] query_parse::Error),

    /// Query planning failed: {0}
    #[error("Query planning failed: {0}")]
    QueryPlan(#[from] query_plan::Error),

    /// JSON extraction failed: {0}
    #[error("JSON extraction failed: {0}")]
    JsonExtraction(String),

    /// Downstream service error: {0}
    #[error("Downstream service error: {0}")]
    Downstream(#[from] BoxError),
}

#[derive(Clone, Debug)]
pub struct PrepareQueryLayer<P, Pl> {
    query_parse_service: P,
    query_plan_service: Pl,
}

impl<P, Pl> PrepareQueryLayer<P, Pl> {
    pub fn new(query_parse_service: P, query_plan_service: Pl) -> Self {
        Self {
            query_parse_service,
            query_plan_service,
        }
    }
}

impl<S, P, Pl> Layer<S> for PrepareQueryLayer<P, Pl> 
where
    P: Clone,
    Pl: Clone,
{
    type Service = PrepareQueryService<S, P, Pl>;

    fn layer(&self, service: S) -> Self::Service {
        PrepareQueryService { 
            inner: service,
            query_parse_service: self.query_parse_service.clone(),
            query_plan_service: self.query_plan_service.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PrepareQueryService<S, P, Pl> {
    inner: S,
    query_parse_service: P,
    query_plan_service: Pl,
}

impl<S, P, Pl> Service<JsonRequest> for PrepareQueryService<S, P, Pl>
where
    S: Service<ExecutionRequest, Response = ExecutionResponse> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    P: Service<query_parse::Request, Response = query_parse::Response> + Clone + Send + 'static,
    P::Future: Send + 'static,
    P::Error: Into<BoxError>,
    Pl: QueryPlanning + Clone + Send + 'static,
{
    type Response = JsonResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: JsonRequest) -> Self::Future {
        // This layer orchestrates query_parse and query_plan services
        // Flow: JSON Request → query_parse → ExecutableDocument → query_plan → QueryPlan → Execution Request
        
        // Create an extended layer for the inner service
        let original_extensions = req.extensions;
        let extended_extensions = original_extensions.extend();

        // Extract query string, operation name and variables from JSON body
        let (query_string, operation_name, query_variables) = match extract_query_details(&req.body) {
            Ok(details) => details,
            Err(e) => return Box::pin(async move { Err(e.into()) }),
        };

        // Clone services for async usage
        let mut query_parse_service = self.query_parse_service.clone();
        let mut query_plan_service = self.query_plan_service.clone();
        let mut inner_service = self.inner.clone();

        Box::pin(async move {
            // TODO: For now, create a placeholder query plan until we implement proper service orchestration
            // In a complete implementation, this would:
            // 1. Call query_parse_service.call(parse_req).await to parse the GraphQL query
            // 2. Call query_plan_service.call(plan_req).await to create the query plan
            // 3. Combine the results into an ExecutionRequest
            
            // Create a placeholder query plan - this will be replaced with actual service calls
            let query_plan = apollo_federation::query_plan::QueryPlan::default();

            let execution_req = ExecutionRequest {
                extensions: extended_extensions,
                operation_name,
                query_plan,
                query_variables,
            };

            // Call the inner execution service
            let execution_resp = inner_service.call(execution_req).await.map_err(Into::into)?;

            // Transform ExecutionResponse back to JsonResponse
            let json_resp = JsonResponse {
                extensions: original_extensions,
                responses: execution_resp.responses,
            };

            Ok(json_resp)
        })
    }
}

fn extract_query_details(json_body: &crate::json::JsonValue) -> Result<(serde_json::Value, Option<String>, std::collections::HashMap<String, serde_json::Value>), Error> {
    // Extract the GraphQL query string
    let query_string = json_body.get("query")
        .ok_or_else(|| Error::JsonExtraction("Missing 'query' field in JSON body".to_string()))?
        .clone();

    // Extract operation name (optional)
    let operation_name = json_body.get("operationName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Extract variables (optional)
    let query_variables = json_body.get("variables")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    Ok((query_string, operation_name, query_variables))
}

#[cfg(test)]
mod tests; 