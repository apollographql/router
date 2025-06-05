use crate::Context;
use apollo_compiler::ExecutableDocument;
use serde_json::Value;
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

pub struct Request {
    pub context: Context,
    pub operation_name: Option<String>,
    pub query: Value,
}

pub struct Response {
    pub context: Context,
    pub operation_name: Option<String>,
    pub query: ExecutableDocument,
}

#[derive(Debug, Error)]
enum Error {}
