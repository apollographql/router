//! Implements GraphQL schema introspection.
use std::future::Ready;
use std::num::NonZeroUsize;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::future::Either;
use sha2::Digest;
use sha2::Sha256;
use tower::ServiceBuilder;
use tower::ServiceExt as _;
use tower::util::BoxCloneService;

use crate::Configuration;
use crate::cache::storage::CacheStorage;
use crate::compute_job;
use crate::compute_job::ComputeBackPressureError;
use crate::compute_job::ComputeJobType;
use crate::graphql;
use crate::json_ext::Object;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::spec;
use crate::spec::QueryHash;

const DEFAULT_INTROSPECTION_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();

#[derive(Clone)]
enum Mode {
    Disabled,
    Enabled {
        storage: Arc<CacheStorage<IntrospectionCacheKey, graphql::Response>>,
        max_depth: MaxDepth,
    },
}

#[derive(Copy, Clone)]
enum MaxDepth {
    Check,
    Ignore,
}

/// Determine the introspection mode based on YAML configuration.
fn introspection_mode(configuration: &Configuration) -> Mode {
    if configuration.supergraph.introspection {
        let storage = Arc::new(CacheStorage::new_in_memory(
            DEFAULT_INTROSPECTION_CACHE_CAPACITY,
            "introspection",
        ));
        Mode::Enabled {
            storage,
            max_depth: if configuration.limits.router.introspection_max_depth {
                MaxDepth::Check
            } else {
                MaxDepth::Ignore
            },
        }
    } else {
        Mode::Disabled
    }
}

/// Request type for [IntrospectionService].
pub(crate) struct IntrospectionRequest {
    /// The GraphQL schema to introspect.
    pub(crate) schema: Arc<spec::Schema>,
    /// Document representing the introspection operation to execute.
    pub(crate) document: ParsedDocument,
    /// JSON variable values used to execute the query.
    pub(crate) variables: Object,
}

/// In-memory cache storage for introspection.
pub(crate) type IntrospectionCache = Arc<CacheStorage<IntrospectionCacheKey, graphql::Response>>;

/// A terminal service that handles (partial) execution of introspection.
pub(crate) type IntrospectionService =
    BoxCloneService<IntrospectionRequest, graphql::Response, ComputeBackPressureError>;

/// Returns a terminal service that does cached, partial execution of introspection.
///
/// If introspection is disabled in config, always returns an error response.
/// If a query contains both introspection and concrete fields, returns an error response.
///
/// Returns the cache object separately for telemetry activation.
pub(crate) fn introspection_service(
    configuration: &Configuration,
) -> (IntrospectionService, Option<IntrospectionCache>) {
    let builder = ServiceBuilder::new().layer(RejectMixedIntrospectionLayer::new());

    match introspection_mode(configuration) {
        Mode::Enabled { storage, max_depth } => (
            builder
                .layer(IntrospectionCacheLayer::new(storage.clone()))
                .service(IntrospectionExecutionService::new(max_depth))
                .boxed_clone(),
            Some(storage),
        ),
        Mode::Disabled => (
            builder
                .service(IntrospectionDisabledService::new())
                .boxed_clone(),
            None,
        ),
    }
}

/// Returns if the document contains an introspection query.
///
/// That is:
/// - The operation is a query operation, AND:
///   - The operation has schema introspection fields (__schema or __type), OR:
///   - The operation does not have explicit root fields (...which implies __typename).
pub(crate) fn is_introspection_query(document: &ParsedDocument) -> bool {
    document.operation.is_query()
        && (document.has_schema_introspection || !document.has_explicit_root_fields)
}

/// Terminal service for GraphQL introspection queries that always returns an error saying
/// introspection is disabled.
#[derive(Clone)]
struct IntrospectionDisabledService {
    _private: (),
}

impl IntrospectionDisabledService {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl tower::Service<IntrospectionRequest> for IntrospectionDisabledService {
    // Actually Infallible, but this matches the IntrospectionExecutionService.
    type Error = ComputeBackPressureError;
    type Response = graphql::Response;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: IntrospectionRequest) -> Self::Future {
        let error = graphql::Error::builder()
            .message(String::from("introspection has been disabled"))
            .extension_code("INTROSPECTION_DISABLED")
            .build();
        std::future::ready(Ok(graphql::Response::builder().error(error).build()))
    }
}

/// Short-circuits requests that contain both introspection and concrete fields, responding with GraphQL errors.
struct RejectMixedIntrospectionLayer {
    _private: (),
}
impl RejectMixedIntrospectionLayer {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for RejectMixedIntrospectionLayer {
    type Service = RejectMixedIntrospectionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RejectMixedIntrospectionService { inner }
    }
}

/// Short-circuits requests that contain both introspection and concrete fields, responding with GraphQL errors.
#[derive(Clone)]
struct RejectMixedIntrospectionService<S> {
    inner: S,
}

impl<S> tower::Service<IntrospectionRequest> for RejectMixedIntrospectionService<S>
where
    S: tower::Service<IntrospectionRequest, Response = graphql::Response>,
{
    type Response = graphql::Response;
    type Error = S::Error;
    type Future = Either<
        S::Future,
        // Rejection
        Ready<Result<Self::Response, Self::Error>>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: IntrospectionRequest) -> Self::Future {
        if req.document.has_schema_introspection && req.document.has_explicit_root_fields {
            let error = graphql::Error::builder()
                .message(
                    "\
                    Mixed queries with both schema introspection and concrete fields \
                    are not supported yet: https://github.com/apollographql/router/issues/2789\
                ",
                )
                .extension_code("MIXED_INTROSPECTION")
                .build();
            Either::Right(std::future::ready(Ok(graphql::Response::builder()
                .error(error)
                .build())))
        } else {
            Either::Left(self.inner.call(req))
        }
    }
}

impl IntrospectionRequest {
    fn is_root_typename_only(&self) -> bool {
        // `has_schema_introspection` is about __type and __schema,
        // so if we don't have either of those AND we don't have explicit root fields, we can
        // assume that we only have `__typename` which is covered by neither.
        !self.document.has_schema_introspection && !self.document.has_explicit_root_fields
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct IntrospectionCacheKey {
    /// Hash of the GraphQL query against a specific schema.
    operation: Arc<QueryHash>,
    /// Hash of the variables used to execute the introspection query.
    variables: sha2::digest::Output<Sha256>,
}

impl std::fmt::Display for IntrospectionCacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "introspect:{}:variables:{:x}",
            self.operation, self.variables
        )
    }
}

/// In-memory caching service for introspection requests.
///
/// A stopgap solution until Apollo Platform provides apollo-cache-memory!
#[derive(Clone)]
struct IntrospectionCacheService<S> {
    inner: S,
    cache: Arc<CacheStorage<IntrospectionCacheKey, graphql::Response>>,
}

/// In-memory caching layer for introspection requests. It uses a fixed cache size.
impl<S> IntrospectionCacheService<S> {
    fn new(inner: S, cache: Arc<CacheStorage<IntrospectionCacheKey, graphql::Response>>) -> Self {
        Self { inner, cache }
    }
}

///
/// A stopgap solution until Apollo Platform provides apollo-cache-memory!
struct IntrospectionCacheLayer {
    cache: Arc<CacheStorage<IntrospectionCacheKey, graphql::Response>>,
}

impl IntrospectionCacheLayer {
    fn new(cache: Arc<CacheStorage<IntrospectionCacheKey, graphql::Response>>) -> Self {
        Self { cache }
    }
}

impl<S> tower::Layer<S> for IntrospectionCacheLayer {
    type Service = IntrospectionCacheService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        IntrospectionCacheService::new(inner, self.cache.clone())
    }
}

impl<S> tower::Service<IntrospectionRequest> for IntrospectionCacheService<S>
where
    S: tower::Service<IntrospectionRequest, Response = graphql::Response> + Clone + Send + 'static,
    S::Error: Send,
    S::Future: Send + 'static,
{
    type Response = graphql::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // This should indicate _cache_ readiness--we don't have that concept right now though.
        // We might not use the inner service so we ready it only on cache misses.
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: IntrospectionRequest) -> Self::Future {
        let mut inner = self.inner.clone();
        let cache = self.cache.clone();

        Box::pin(async move {
            let cache_key = if let Ok(variable_key) = serde_json::to_string(&req.variables) {
                let mut hasher = Sha256::new();
                hasher.update(variable_key);
                IntrospectionCacheKey {
                    operation: req.document.hash.clone(),
                    variables: hasher.finalize(),
                }
            } else {
                tracing::warn!(
                    "Failed to serialize variables for introspection cache key, skipping cache: {:?}",
                    req.variables
                );

                return inner.ready().await?.call(req).await;
            };

            if let Some(response) = cache.get(&cache_key, |_| unreachable!()).await {
                return Ok(response);
            }

            let response = inner.ready().await?.call(req).await?;
            cache.insert(cache_key, response.clone()).await;

            Ok(response)
        })
    }
}

/// Terminal service for executing GraphQL introspection queries against a schema.
///
/// Only the introspection parts of the input GraphQL query are executed. Non-introspection parts
/// are silently ignored.
///
/// When the introspection depth limit is exceeded, returns an error response.
#[derive(Clone)]
struct IntrospectionExecutionService {
    max_depth: MaxDepth,
}

impl IntrospectionExecutionService {
    fn new(max_depth: MaxDepth) -> Self {
        Self { max_depth }
    }
}

impl tower::Service<IntrospectionRequest> for IntrospectionExecutionService {
    type Response = graphql::Response;
    type Error = ComputeBackPressureError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: IntrospectionRequest) -> Self::Future {
        let max_depth = self.max_depth;

        Box::pin(async move {
            // Don't go through the heavy-duty compute job pool just to return "Query".
            let response = if req.is_root_typename_only() {
                execute_introspection(
                    // No list field so depth is already known to be zero:
                    MaxDepth::Ignore,
                    &req.schema,
                    &req.document,
                    req.variables,
                )
            } else {
                compute_job::execute(ComputeJobType::Introspection, move |_| {
                    execute_introspection(max_depth, &req.schema, &req.document, req.variables)
                })?
                .await
            };

            Ok(response)
        })
    }
}

fn execute_introspection(
    max_depth: MaxDepth,
    schema: &spec::Schema,
    doc: &ParsedDocument,
    variables: Object,
) -> graphql::Response {
    let api_schema = schema.api_schema();
    let operation = &doc.operation;
    let max_depth_result = match max_depth {
        MaxDepth::Check => {
            apollo_compiler::introspection::check_max_depth(&doc.executable, operation)
        }
        MaxDepth::Ignore => Ok(()),
    };
    let result = max_depth_result
        .and_then(|()| {
            apollo_compiler::request::coerce_variable_values(api_schema, operation, &variables)
        })
        .and_then(|variable_values| {
            apollo_compiler::introspection::partial_execute(
                api_schema,
                &schema.implementers_map,
                &doc.executable,
                operation,
                &variable_values,
            )
        });
    match result {
        Ok(response) => response.into(),
        Err(e) => {
            let error = e.to_graphql_error(&doc.executable.sources);
            graphql::Response::builder().error(error).build()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::IntrospectionCacheLayer;
    use super::IntrospectionRequest;
    use super::introspection_service;
    use crate::Configuration;
    use crate::cache::storage::CacheStorage;
    use crate::graphql;
    use crate::spec::Query;
    use crate::spec::Schema;

    #[tokio::test]
    async fn introspection_cache_hit() {
        let (mock, mut handle) =
            tower_test::mock::pair::<IntrospectionRequest, graphql::Response>();
        let driver = tokio::task::spawn(async move {
            let (_request, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                graphql::Response::builder()
                    .data(serde_json_bytes::json!({
                        "__schema": {
                            "queryType": {
                                "name": "Query",
                            },
                        },
                    }))
                    .build(),
            );
        });

        let config = Configuration::default();
        let schema =
            Arc::new(Schema::parse(include_str!("testdata/supergraph.graphql"), &config).unwrap());
        let query = "{ __schema { queryType { name } } }";

        let cache = Arc::new(CacheStorage::new_in_memory(
            NonZeroUsize::new(5).unwrap(),
            "introspection",
        ));
        let mut service = ServiceBuilder::new()
            .layer(IntrospectionCacheLayer::new(cache))
            .service(mock);

        // We should be able to call the mock service twice with the same query, despite only handling
        // one request.

        let document = Query::parse_document(query, None, &schema, &config).unwrap();
        service
            .ready()
            .await
            .unwrap()
            .call(IntrospectionRequest {
                schema: schema.clone(),
                document,
                variables: Default::default(),
            })
            .await
            .unwrap();

        let document = Query::parse_document(query, None, &schema, &config).unwrap();
        service
            .ready()
            .await
            .unwrap()
            .call(IntrospectionRequest {
                schema: schema.clone(),
                document,
                variables: Default::default(),
            })
            .await
            .unwrap();

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_single_aliased_root_typename() {
        let mut config = Configuration::default();
        config.supergraph.introspection = true;
        let schema =
            Arc::new(Schema::parse(include_str!("testdata/supergraph.graphql"), &config).unwrap());
        let query = "{ x: __typename }";
        let document = Query::parse_document(query, None, &schema, &config).unwrap();

        let (service, _cache) = introspection_service(&config);
        let response = service
            .oneshot(IntrospectionRequest {
                schema,
                document,
                variables: Default::default(),
            })
            .await
            .unwrap();

        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                "x": "Query",
            })),
        );
    }

    #[tokio::test]
    async fn test_two_root_typenames() {
        let mut config = Configuration::default();
        config.supergraph.introspection = true;

        let schema =
            Arc::new(Schema::parse(include_str!("testdata/supergraph.graphql"), &config).unwrap());
        let query = "{ x: __typename __typename }";
        let document = Query::parse_document(query, None, &schema, &config).unwrap();

        let (service, _cache) = introspection_service(&config);
        let response = service
            .oneshot(IntrospectionRequest {
                schema,
                document,
                variables: Default::default(),
            })
            .await
            .unwrap();

        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                "x": "Query",
                "__typename": "Query",
            })),
        );
    }
}
