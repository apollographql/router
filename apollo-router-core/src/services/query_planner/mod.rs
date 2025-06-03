use apollo_compiler::ExecutableDocument;
use apollo_federation::query_plan::QueryPlan;
use serde_json::Value;
use services::context::Context;
use std::collections::HashMap;
use thiserror::Error;
use tower::util::BoxCloneService;
use tower::BoxError;

pub struct Request {
    pub context: Context,
    pub operation_name: Option<String>,
    pub query: ExecutableDocument,
}

pub struct Response {
    pub context: Context,
    pub operation_name: Option<String>,

    // TODO maybe wrap to make immutable
    pub query_plan: QueryPlan,
}

#[derive(Debug, Error)]
enum Error {}

type QueryPlannerService = BoxCloneService<Request, Result<Response, Error>, BoxError>;
