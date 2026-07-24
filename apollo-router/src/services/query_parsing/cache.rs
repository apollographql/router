use std::num::NonZeroUsize;
use std::sync::Arc;

use futures::future::BoxFuture;
use lru::LruCache;
use tokio::sync::Mutex;

use crate::compute_job::MaybeBackPressureError;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::query_parsing::Request;
use crate::services::query_parsing::ServiceError;
use crate::spec::SpecError;

/// In-memory caching for GraphQL query parsing.
///
/// A stopgap solution until Apollo Platform provides apollo-cache-memory!
#[derive(Clone)]
pub(crate) struct QueryParsingCacheLayer {
    cache: Arc<Mutex<LruCache<Request, Result<ParsedDocument, SpecError>>>>,
}

impl QueryParsingCacheLayer {
    pub(crate) fn new(limit: NonZeroUsize) -> Self {
        Self {
            cache: Arc::new(Mutex::new(LruCache::new(limit))),
        }
    }
}

impl<S> tower::Layer<S> for QueryParsingCacheLayer {
    type Service = QueryParsingCacheService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        QueryParsingCacheService {
            inner,
            cache: self.cache.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct QueryParsingCacheService<S> {
    inner: S,
    cache: Arc<Mutex<LruCache<Request, Result<ParsedDocument, SpecError>>>>,
}

impl<S> tower::Service<Request> for QueryParsingCacheService<S>
where
    S: tower::Service<Request, Response = ParsedDocument, Error = ServiceError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = ParsedDocument;
    type Error = ServiceError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let cache = self.cache.clone();

        Box::pin(async move {
            if let Some(cached) = cache.lock().await.get(&req).cloned() {
                return cached.map_err(MaybeBackPressureError::PermanentError);
            }

            let key = req.clone();
            match inner.call(req).await {
                Ok(doc) => {
                    cache.lock().await.put(key, Ok(doc.clone()));
                    Ok(doc)
                }
                Err(MaybeBackPressureError::PermanentError(err)) => {
                    cache.lock().await.put(key, Err(err.clone()));
                    Err(MaybeBackPressureError::PermanentError(err))
                }
                Err(other) => Err(other),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use futures::future::BoxFuture;
    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Configuration;
    use crate::compute_job::ComputeBackPressureError;

    const SCHEMA: &str = include_str!("../../../testing_schema.graphql");

    /// Wrap a tower-test mock so it can be used as a query parsing service.
    ///
    /// In this case a tower-test error indicates a compute job backpressure error, and an Err
    /// response indicates an error inside the inner service.
    #[derive(Clone)]
    struct MockQueryParsing(tower_test::mock::Mock<Request, Result<ParsedDocument, SpecError>>);

    impl tower::Service<Request> for MockQueryParsing {
        type Response = ParsedDocument;
        type Error = ServiceError;
        type Future = BoxFuture<'static, Result<ParsedDocument, ServiceError>>;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0
                .poll_ready(cx)
                .map_err(|_| MaybeBackPressureError::TemporaryError(ComputeBackPressureError))
        }

        fn call(&mut self, req: Request) -> Self::Future {
            let inner = self.0.clone();
            let mut inner = std::mem::replace(&mut self.0, inner);
            Box::pin(async move {
                match inner.call(req).await {
                    Ok(Ok(doc)) => Ok(doc),
                    Ok(Err(spec_error)) => Err(MaybeBackPressureError::PermanentError(spec_error)),
                    Err(_boxed) => Err(MaybeBackPressureError::TemporaryError(
                        ComputeBackPressureError,
                    )),
                }
            })
        }
    }

    fn mock_pair() -> (
        MockQueryParsing,
        tower_test::mock::Handle<Request, Result<ParsedDocument, SpecError>>,
    ) {
        let (mock, handle) = tower_test::mock::pair();
        (MockQueryParsing(mock), handle)
    }

    #[tokio::test]
    async fn same_query_is_cache_hit() {
        let config = Configuration::default();
        let schema = crate::spec::Schema::parse(SCHEMA, &config).unwrap();

        let (mock, mut handle) = mock_pair();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .service(mock);

        let req = Request::new("query { me { id } }".to_string(), None);

        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(crate::spec::Query::parse_document(
                &req.query, None, &schema, &config,
            ));
            handle
        });

        let first = service
            .ready()
            .await
            .unwrap()
            .call(req.clone())
            .await
            .unwrap();
        let handle = driver.await.unwrap();

        // Same request again should work despite mock only allowing one request
        let second = service.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(second.hash, first.hash);
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn different_query_is_cache_miss() {
        let config = Configuration::default();
        let schema = crate::spec::Schema::parse(SCHEMA, &config).unwrap();

        let (mock, mut handle) = mock_pair();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .service(mock);

        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(crate::spec::Query::parse_document(
                &req.query, None, &schema, &config,
            ));
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(crate::spec::Query::parse_document(
                &req.query, None, &schema, &config,
            ));
        });

        let a = service
            .ready()
            .await
            .unwrap()
            .call(Request::new("query { me { id } }".to_string(), None))
            .await
            .unwrap();
        let b = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(
                "query { topProducts { name } }".to_string(),
                None,
            ))
            .await
            .unwrap();

        assert_ne!(a.hash, b.hash);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn parse_error_is_cached() {
        let (mock, mut handle) = mock_pair();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .service(mock);
        let req = Request::new("query { me { id } }".to_string(), None);

        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            // HACK(@goto-bus-stop): This is turned into a PermanentError by the `MockQueryParsing`
            // helper.
            responder.send_response(Err(SpecError::UnknownOperation("a".to_string())));
            handle
        });

        let err = service
            .ready()
            .await
            .unwrap()
            .call(req.clone())
            .await
            .expect_err("should fail");
        assert!(matches!(err, MaybeBackPressureError::PermanentError(_)));
        let handle = driver.await.unwrap();

        // We should be able to get the error from cache on a second attempt
        let err = service
            .ready()
            .await
            .unwrap()
            .call(req)
            .await
            .expect_err("should fail again from cache");
        assert!(matches!(err, MaybeBackPressureError::PermanentError(_)));
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn backpressure_error_is_not_cached() {
        let config = Configuration::default();
        let schema = crate::spec::Schema::parse(SCHEMA, &config).unwrap();

        let (mock, mut handle) = mock_pair();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .service(mock);
        let req = Request::new("query { me { id } }".to_string(), None);

        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_error("temporarily overloaded");

            // We expect another request since the error is not cached!
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(crate::spec::Query::parse_document(
                &req.query, None, &schema, &config,
            ));
        });

        let err = service
            .ready()
            .await
            .unwrap()
            .call(req.clone())
            .await
            .expect_err("should fail");
        assert!(matches!(err, MaybeBackPressureError::TemporaryError(_)));

        let ok = service.ready().await.unwrap().call(req).await;
        assert!(ok.is_ok());
        crate::plugin::test::await_mock_driver(driver).await;
    }
}
