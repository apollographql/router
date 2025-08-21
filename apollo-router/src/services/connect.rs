//! Connect service request and response types.

use std::fmt::Debug;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use parking_lot::Mutex;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::Context;
use crate::graphql;
use crate::graphql::Request as GraphQLRequest;
use crate::plugins::connectors::make_requests::make_requests;
use crate::query_planner::fetch::Variables;
use crate::services::connector::request_service::Request as ConnectorRequest;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

#[non_exhaustive]
pub struct Request {
    pub(crate) service_name: Arc<str>,
    pub(crate) context: Context,
    pub(crate) prepared_requests: Vec<ConnectorRequest>,
    /// Subgraph name needed for lazy cache key generation
    pub(crate) subgraph_name: String,
}

impl Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("service_name", &self.service_name)
            .field("context", &self.context)
            .field("subgraph_name", &self.subgraph_name)
            .field("prepared_requests_len", &self.prepared_requests.len())
            .finish()
    }
}

assert_impl_all!(Response: Send);
#[derive(Debug)]
#[non_exhaustive]
pub struct Response {
    pub(crate) response: http::Response<graphql::Response>,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        service_name: Arc<str>,
        context: Context,
        operation: Arc<Valid<ExecutableDocument>>,
        supergraph_request: Arc<http::Request<GraphQLRequest>>,
        variables: Variables,
        keys: Option<Valid<FieldSet>>,
        connector: Arc<Connector>,
    ) -> Result<Self, BoxError> {
        // Get debug context from context extensions
        let debug = context
            .extensions()
            .with_lock(|lock| lock.get::<Arc<Mutex<ConnectorContext>>>().cloned());

        // Call make_requests to prepare HTTP requests
        let prepared_requests = make_requests(
            &operation,
            &variables,
            keys.as_ref(),
            &context,
            supergraph_request.clone(),
            connector.clone(),
            &debug,
        )
        .map_err(BoxError::from)?;

        // Store subgraph name for lazy cache key generation
        let subgraph_name = connector.id.subgraph_name.to_string();

        Ok(Self {
            service_name,
            context: context.clone(),
            prepared_requests,
            subgraph_name,
        })
    }
}
