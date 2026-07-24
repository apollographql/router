//! A tower service for GraphQL query parsing and validation.

use std::hash::Hash;
use std::sync::Arc;

use tower::ServiceBuilder;
use tower::ServiceExt as _;

use crate::Configuration;
use crate::compute_job::ComputeJobType;
use crate::compute_job::MaybeBackPressureError;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::query_parsing::cache::QueryParsingCacheLayer;
use crate::services::query_parsing::recursive_selections_limit::LimitRecursiveSelectionLayer;
use crate::services::query_parsing::service::QueryParsingService;
use crate::spec::Schema;
use crate::spec::SpecError;

pub(crate) mod cache;
pub(crate) mod recursive_selections_limit;
pub(crate) mod service;

/// Request to parse and validate a GraphQL query.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,
    /// The priority to use on the compute job pool. This field does not participate in
    /// hashing or equality for caching.
    pub(crate) compute_job_type: ComputeJobType,
}

impl Hash for Request {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.query.hash(state);
        self.operation_name.hash(state);
    }
}

impl PartialEq for Request {
    fn eq(&self, other: &Self) -> bool {
        self.query == other.query && self.operation_name == other.operation_name
    }
}

impl Eq for Request {}

impl Request {
    /// Create a parse request for a GraphQL query.
    pub(crate) fn new(query: String, operation_name: Option<String>) -> Self {
        Self {
            query,
            operation_name,
            compute_job_type: ComputeJobType::QueryParsing,
        }
    }

    /// Create a low-priority parse request for a GraphQL query.
    pub(crate) fn new_warmup(query: String, operation_name: Option<String>) -> Self {
        Self {
            query,
            operation_name,
            compute_job_type: ComputeJobType::QueryParsingWarmup,
        }
    }
}

pub(crate) type ServiceError = MaybeBackPressureError<SpecError>;
pub(crate) type BoxCloneService =
    tower::util::BoxCloneService<Request, ParsedDocument, ServiceError>;

/// Build a query parsing service with caching.
pub(crate) fn query_parsing_service(
    schema: Arc<Schema>,
    configuration: Arc<Configuration>,
) -> BoxCloneService {
    let cache_limit = configuration
        .supergraph
        .query_planning
        .cache
        .in_memory
        .limit;
    let max_recursive_selections = configuration.limits.router.max_recursive_selections;
    let warn_only = configuration.limits.router.warn_only;

    ServiceBuilder::new()
        .layer(QueryParsingCacheLayer::new(cache_limit))
        .layer(LimitRecursiveSelectionLayer::new(
            max_recursive_selections,
            warn_only,
        ))
        .service(QueryParsingService::new(schema, configuration))
        .boxed_clone()
}
