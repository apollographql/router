//! A tower service for GraphQL query parsing and validation.

use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Operation;
use apollo_compiler::validation::Valid;
use tower::ServiceBuilder;
use tower::ServiceExt as _;

use self::cache::QueryParsingCacheLayer;
use self::recursive_selections_limit::LimitRecursiveSelectionLayer;
use self::service::QueryParsingService;
use crate::Configuration;
use crate::compute_job::ComputeJobType;
use crate::compute_job::MaybeBackPressureError;
use crate::spec::QueryHash;
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

pub(crate) type ParsedDocument = Arc<ParsedDocumentInner>;

#[derive(Debug)]
pub(crate) struct ParsedDocumentInner {
    pub(crate) ast: ast::Document,
    pub(crate) executable: Arc<Valid<ExecutableDocument>>,
    pub(crate) hash: Arc<QueryHash>,
    pub(crate) operation: Node<Operation>,
}

impl ParsedDocumentInner {
    pub(crate) fn new(
        ast: ast::Document,
        executable: Arc<Valid<ExecutableDocument>>,
        operation_name: Option<&str>,
        hash: Arc<QueryHash>,
    ) -> Result<Arc<Self>, SpecError> {
        let operation = get_operation(&executable, operation_name)?;
        Ok(Arc::new(Self {
            ast,
            executable,
            hash,
            operation,
        }))
    }
}

pub(crate) fn get_operation(
    executable: &ExecutableDocument,
    operation_name: Option<&str>,
) -> Result<Node<Operation>, SpecError> {
    if let Ok(operation) = executable.operations.get(operation_name) {
        Ok(operation.clone())
    } else if let Some(name) = operation_name {
        Err(SpecError::UnknownOperation(name.to_owned()))
    } else if executable.operations.is_empty() {
        // Maybe not reachable?
        // A valid document is non-empty and has no unused fragments
        Err(SpecError::NoOperation)
    } else {
        debug_assert!(executable.operations.len() > 1);
        Err(SpecError::MultipleOperationWithoutOperationName)
    }
}

impl Display for ParsedDocumentInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Hash for ParsedDocumentInner {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl PartialEq for ParsedDocumentInner {
    fn eq(&self, other: &Self) -> bool {
        self.ast == other.ast
    }
}

impl Eq for ParsedDocumentInner {}
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
