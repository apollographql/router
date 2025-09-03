//! Connect service request and response types.

use std::fmt::Debug;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::cache::CachePolicy;
use apollo_federation::connectors::runtime::cache::CacheableIterator;
use apollo_federation::connectors::runtime::cache::create_cacheable_iterator;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use apollo_federation::connectors::runtime::key::ResponseKey;
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
    #[allow(dead_code)]
    pub(crate) variables: Variables,
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
    pub(crate) cache_policy: CachePolicy,
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
            variables,
            subgraph_name,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_new(prepared_requests: Vec<ConnectorRequest>) -> Self {
        Self {
            service_name: Arc::from("test_service"),
            context: Context::default(),
            prepared_requests,
            variables: Default::default(),
            subgraph_name: "test_subgraph".into(),
        }
    }

    /// Get an iterator over cacheable items with consolidation logic applied.
    ///
    /// Returns an iterator that:
    /// - Consolidates multiple RootField requests into a single cacheable unit
    /// - Emits one item per Entity/EntityField request for independent caching
    /// - Materializes BatchEntity requests into separate items per batch range
    pub fn cacheable_items(&self) -> CacheableIterator {
        let requests: Vec<(ResponseKey, TransportRequest)> = self
            .prepared_requests
            .iter()
            .map(|req| (req.key.clone(), req.transport_request.clone()))
            .collect();
        create_cacheable_iterator(requests, &self.subgraph_name)
    }
}

impl Response {
    /// Create a new Response with the given HTTP response and cache policy
    pub fn new(response: http::Response<graphql::Response>, cache_policy: CachePolicy) -> Self {
        Self {
            response,
            cache_policy,
        }
    }

    /// Create a new Response with default cache policy (no caching)
    pub fn with_default_cache_policy(response: http::Response<graphql::Response>) -> Self {
        Self {
            response,
            cache_policy: CachePolicy::Roots(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_new() -> Self {
        Self::with_default_cache_policy(
            http::Response::builder()
                .body(graphql::Response::default())
                .unwrap(),
        )
    }
}
