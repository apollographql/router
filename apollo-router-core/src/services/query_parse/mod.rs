use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use serde_json::Value;

pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: Value,
}

pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: ExecutableDocument,
}
