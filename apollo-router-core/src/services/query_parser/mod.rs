use apollo_compiler::ExecutableDocument;
use serde_json::Value;
use services::context::Context;
use std::collections::HashMap;
use thiserror::Error;
use tower::util::BoxCloneService;
use tower::BoxError;

pub struct Request {
    pub context: Context,
    pub operation_name: Option<String>,
    pub query: String,
}

pub struct Response {
    pub context: Context,
    pub operation_name: Option<String>,
    pub query: ExecutableDocument,
}

#[derive(Debug, Error)]
enum Error {}

type QueryParserService = BoxCloneService<Request, Result<Response, Error>, BoxError>;
