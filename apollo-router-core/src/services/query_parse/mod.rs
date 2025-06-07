use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::validation::{Valid, WithErrors};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: String,
}

pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>>,
}

/// Query parsing service that transforms query strings into parsed ExecutableDocuments with validation
#[derive(Clone, Debug)]
pub struct QueryParseService {
    schema: Valid<Schema>,
}

impl QueryParseService {
    pub fn new(schema: Valid<Schema>) -> Self {
        Self { schema }
    }

    /// Parse a GraphQL query string into a Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>>
    /// 
    /// This method uses apollo_compiler's parse_and_validate. On success, it returns the
    /// Valid<ExecutableDocument> directly. On failure, it returns the WithErrors<ExecutableDocument>.
    fn parse_query(&self, query_string: &str) -> Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>> {
        // Parse and validate the GraphQL query using apollo_compiler
        // This returns Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>>
        ExecutableDocument::parse_and_validate(&self.schema, query_string, "query.graphql")
    }
}

impl Service<Request> for QueryParseService {
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let query_string = req.query.clone();
        let operation_name = req.operation_name.clone();
        let extensions = req.extensions;
        let service = self.clone();

        Box::pin(async move {
            // Parse the query, returning Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>>
            let parsed_query = service.parse_query(query_string.as_str());

            Ok(Response {
                extensions,
                operation_name,
                query: parsed_query,
            })
        })
    }
}

#[cfg(test)]
mod tests;
