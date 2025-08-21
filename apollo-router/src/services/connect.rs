//! Connect service request and response types.

use std::fmt::Debug;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::cache::ConnectorCachePolicy;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use parking_lot::Mutex;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::Context;
use crate::graphql;
use crate::graphql::Request as GraphQLRequest;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::connectors::make_requests::make_requests;
use crate::query_planner::fetch::Variables;
use crate::services::connector::request_service::Request as ConnectorRequest;
use crate::spec::QueryHash;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

#[non_exhaustive]
pub struct Request {
    pub(crate) service_name: Arc<str>,
    pub(crate) context: Context,
    pub(crate) prepared_requests: Vec<ConnectorRequest>,
    /// Subgraph name needed for lazy cache key generation
    pub(crate) subgraph_name: String,

    /// Cache-related fields for connector response caching
    pub(crate) query_hash: Arc<QueryHash>,
    /// Authorization metadata for cache key generation
    pub(crate) authorization: Arc<CacheKeyMetadata>,

    // Legacy fields for backward compatibility with tests - these will be removed in a future PR
    #[deprecated]
    #[allow(dead_code)]
    pub(crate) operation: Arc<Valid<ExecutableDocument>>,
    #[deprecated]
    #[allow(dead_code)]
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    #[deprecated]
    #[allow(dead_code)]
    pub(crate) variables: Variables,
    #[deprecated]
    #[allow(dead_code)]
    pub(crate) keys: Option<Valid<FieldSet>>,
}

impl Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("service_name", &self.service_name)
            .field("context", &self.context)
            .field("subgraph_name", &self.subgraph_name)
            .field("prepared_requests_len", &self.prepared_requests.len())
            .field("query_hash", &self.query_hash)
            .field("authorization", &self.authorization)
            .finish()
    }
}

assert_impl_all!(Response: Send);
#[derive(Debug)]
#[non_exhaustive]
pub struct Response {
    pub(crate) response: http::Response<graphql::Response>,
    /// Cache policy for connector response caching
    #[allow(dead_code)] // Will be used in PR5
    pub(crate) cache_policy: ConnectorCachePolicy,
}

impl Response {
    /// Create a new Response with the given HTTP response and cache policy
    pub fn new(
        response: http::Response<graphql::Response>,
        cache_policy: ConnectorCachePolicy,
    ) -> Self {
        Self {
            response,
            cache_policy,
        }
    }

    /// Create a new Response with default cache policy (no caching)
    pub fn with_default_cache(response: http::Response<graphql::Response>) -> Self {
        Self {
            response,
            cache_policy: ConnectorCachePolicy::default(),
        }
    }
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
        query_hash: Arc<QueryHash>,
        authorization: Arc<CacheKeyMetadata>,
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
            query_hash,
            authorization,

            // Legacy fields for backward compatibility - simplified version
            #[allow(deprecated)]
            operation: operation.clone(),
            #[allow(deprecated)]
            supergraph_request: supergraph_request.clone(),
            #[allow(deprecated)]
            variables: Variables {
                variables: variables.variables.clone(),
                inverted_paths: variables.inverted_paths.clone(),
                contextual_arguments: None, // Simplified for backward compatibility
            },
            #[allow(deprecated)]
            keys,
        })
    }
}
