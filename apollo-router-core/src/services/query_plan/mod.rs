use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use apollo_federation::query_plan::QueryPlan;

pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: ExecutableDocument,
}

pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,

    // TODO maybe wrap to make immutable
    pub query_plan: QueryPlan,
}
