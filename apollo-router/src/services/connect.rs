//! Connect service request and response types.

use std::fmt::Debug;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use http::HeaderMap;
use parking_lot::Mutex;
use sha2::Digest;
use sha2::Sha256;
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
pub(crate) struct Request {
    pub(crate) service_name: Arc<str>,
    pub(crate) context: Context,
    pub(crate) operation: Arc<Valid<ExecutableDocument>>,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
    #[allow(dead_code)]
    pub(crate) keys: Option<Valid<FieldSet>>,
    #[allow(dead_code)]
    pub(crate) cache_keys: Vec<String>,
    pub(crate) prepared_requests: Vec<ConnectorRequest>,
}

impl Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("service_name", &self.service_name)
            .field("context", &self.context)
            .field("operation", &self.operation)
            .field("supergraph_request", &self.supergraph_request)
            .field("variables", &self.variables.variables)
            .finish()
    }
}

assert_impl_all!(Response: Send);
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct Response {
    pub(crate) response: http::Response<graphql::Response>,
    pub(crate) cache_policies: Vec<HeaderMap>,
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
            connector,
            &debug,
        )
        .map_err(|e| BoxError::from(format!("Failed to prepare connector requests: {}", e)))?;

        // Generate cache keys from prepared requests
        let cache_keys = prepared_requests.iter().map(generate_cache_key).collect();

        Ok(Self {
            service_name,
            context,
            operation,
            supergraph_request,
            variables,
            keys,
            cache_keys,
            prepared_requests,
        })
    }
}

/// Generate a deterministic cache key from a connector request
pub(crate) fn generate_cache_key(request: &ConnectorRequest) -> String {
    let mut hasher = Sha256::new();

    // Include subgraph name for uniqueness across subgraphs
    hasher.update(request.connector.id.subgraph_name.as_bytes());

    match &request.transport_request {
        TransportRequest::Http(http_req) => {
            let req = &http_req.inner;

            // Include HTTP method
            hasher.update(req.method().as_str().as_bytes());

            // Include URI (contains interpolated values)
            hasher.update(req.uri().to_string().as_bytes());

            // Include relevant headers (sorted for determinism)
            // Only include non-sensitive headers that affect the response
            let mut headers: Vec<_> = req
                .headers()
                .iter()
                .filter(|(name, _)| {
                    let name_str = name.as_str().to_lowercase();
                    // Include content-type and custom headers, exclude auth headers
                    name_str.starts_with("x-")
                        || name_str == "content-type"
                        || name_str == "accept"
                        || name_str == "user-agent"
                })
                .collect();
            headers.sort_by_key(|(name, _)| name.as_str());

            for (name, value) in headers {
                hasher.update(name.as_str().as_bytes());
                if let Ok(value_str) = value.to_str() {
                    hasher.update(value_str.as_bytes());
                }
            }

            // Include request body if present
            hasher.update(req.body().as_bytes());
        }
    }

    // Format as connector cache key with version
    format!("connector:v1:{:x}", hasher.finalize())
}
