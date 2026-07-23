//! A tower service for GraphQL query parsing and validation.

use std::sync::Arc;

use tower::ServiceBuilder;
use tower::ServiceExt as _;

use crate::Configuration;
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
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,
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
