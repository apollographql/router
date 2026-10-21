use apollo_cache_memory::InMemoryCacheLayer;
use apollo_cache_memory::InMemoryCacheService;
use apollo_cache_memory::InMemoryConfig;
use tower::util::MapResult;

use crate::compute_job::MaybeBackPressureError;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_parsing::Request;
use crate::services::query_parsing::ServiceError;
use crate::spec::SpecError;

/// Default size of the in-memory cache.
const DEFAULT_CACHE_SIZE: u64 = 512;

/// Cache storage: either a parsed document or a parse error.
type CacheEntry = Result<ParsedDocument, SpecError>;

// Type aliases for the layer's `Service` associated type ... annoying to write out, but this way we
// don't need to box
type SplitFn = fn(Result<ParsedDocument, ServiceError>) -> Result<CacheEntry, ServiceError>;
type FlattenFn = fn(Result<CacheEntry, ServiceError>) -> Result<ParsedDocument, ServiceError>;

/// Only `Ok` responses are cached, and we want to cache parse errors, so they have to be
/// double-Resulted.
fn split_cacheable_parse_error(
    result: Result<ParsedDocument, ServiceError>,
) -> Result<CacheEntry, ServiceError> {
    match result {
        Ok(document) => Ok(Ok(document)),
        Err(MaybeBackPressureError::PermanentError(err)) => Ok(Err(err)),
        Err(err) => Err(err),
    }
}

/// The inverse of [`split_cacheable_parse_error`].
fn flatten_cacheable_parse_error(
    result: Result<CacheEntry, ServiceError>,
) -> Result<ParsedDocument, ServiceError> {
    match result {
        Ok(Ok(document)) => Ok(document),
        Ok(Err(err)) => Err(MaybeBackPressureError::PermanentError(err)),
        Err(err) => Err(err),
    }
}

/// In-memory caching for GraphQL query parsing based on [apollo-cache-memory].
///
/// Parsed documents and parse errors are cached. Backpressure errors are temporary, and are not.
///
/// [apollo-cache-memory]: apollo_cache_memory::InMemoryCacheLayer
#[derive(Clone)]
pub(crate) struct QueryParsingCacheLayer {
    cache: InMemoryCacheLayer<Request, CacheEntry>,
}

impl QueryParsingCacheLayer {
    pub(crate) fn new(config: InMemoryConfig<DEFAULT_CACHE_SIZE>) -> Self {
        Self {
            cache: InMemoryCacheLayer::new("query_parsing", config),
        }
    }
}

impl<S> tower::Layer<S> for QueryParsingCacheLayer
where
    S: tower::Service<Request, Response = ParsedDocument, Error = ServiceError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service =
        MapResult<InMemoryCacheService<MapResult<S, SplitFn>, Request, CacheEntry>, FlattenFn>;

    fn layer(&self, inner: S) -> Self::Service {
        tower::ServiceBuilder::new()
            .map_result(flatten_cacheable_parse_error as FlattenFn)
            .layer(self.cache.clone())
            .map_result(split_cacheable_parse_error as SplitFn)
            .service(inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Configuration;
    use crate::compute_job::ComputeBackPressureError;

    const SCHEMA: &str = include_str!("../../../testing_schema.graphql");

    fn downcast_mock_err(err: tower::BoxError) -> ServiceError {
        *err.downcast()
            .expect("mock should only return ServiceErrors")
    }

    async fn mock_parser(
        mut handle: tower_test::mock::Handle<Request, ParsedDocument>,
        schema: Arc<crate::spec::Schema>,
        config: Arc<Configuration>,
    ) {
        while let Some((req, responder)) = handle.next_request().await {
            match crate::spec::Query::parse_document(
                &req.query,
                req.operation_name.as_deref(),
                &schema,
                &config,
            ) {
                Ok(document) => responder.send_response(document),
                Err(err) => responder.send_error(err),
            }
        }
    }

    #[tokio::test]
    async fn backpressure_error_is_not_cached() {
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SCHEMA, &config).unwrap());

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(InMemoryConfig::<512>::default()))
            .map_err(downcast_mock_err)
            .service(mock);
        let req = Request::new("query { me { id } }".to_string(), None);

        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_error(
                MaybeBackPressureError::TemporaryError(ComputeBackPressureError) as ServiceError,
            );

            // We expect another request since the error is not cached!
            mock_parser(handle, schema, config).await;
        });

        let err = service
            .ready()
            .await
            .unwrap()
            .call(req.clone())
            .await
            .expect_err("should fail");
        assert!(matches!(err, MaybeBackPressureError::TemporaryError(_)));

        let result = service.ready().await.unwrap().call(req).await;
        assert!(result.is_ok());

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }
}
