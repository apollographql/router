use std::num::NonZeroUsize;
use std::sync::Arc;

use futures::future::BoxFuture;
use lru::LruCache;
use tokio::sync::Mutex;

use crate::compute_job::MaybeBackPressureError;
use crate::services::query_parsing::ParsedDocument;
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
    async fn same_query_is_cache_hit() {
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SCHEMA, &config).unwrap());

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .map_err(downcast_mock_err)
            .service(mock);

        let req = Request::new("query { me { id } }".to_string(), None);

        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            match crate::spec::Query::parse_document(
                &req.query,
                req.operation_name.as_deref(),
                &schema,
                &config,
            ) {
                Ok(document) => responder.send_response(document),
                Err(err) => responder.send_error(err),
            }

            // We *do* need to await the next request so that the service can be readied
            assert!(
                handle.next_request().await.is_none(),
                "should not receive a second request"
            );
        });

        let first = service
            .ready()
            .await
            .unwrap()
            .call(req.clone())
            .await
            .unwrap();

        // Same request again should work despite mock only allowing one request
        let second = service.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(second.hash, first.hash);

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn different_query_is_cache_miss() {
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SCHEMA, &config).unwrap());

        let (mock, handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .map_err(downcast_mock_err)
            .service(mock);

        let driver = tokio::spawn(mock_parser(handle, schema, config));

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

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn different_operation_name_is_cache_miss() {
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SCHEMA, &config).unwrap());

        let (mock, handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .map_err(downcast_mock_err)
            .service(mock);

        let driver = tokio::spawn(mock_parser(handle, schema, config));

        let document = r#"
            query UserId {
                me { id }
            }

            query ProductNames {
                topProducts { name }
            }
        "#;

        let a = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(
                document.to_string(),
                Some("UserId".to_string()),
            ))
            .await
            .unwrap();
        let b = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(
                document.to_string(),
                Some("ProductNames".to_string()),
            ))
            .await
            .unwrap();

        assert_ne!(a.hash, b.hash);

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn parse_error_is_cached() {
        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
            .map_err(downcast_mock_err)
            .service(mock);
        let req = Request::new("query { me { id } }".to_string(), None);

        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_error(MaybeBackPressureError::PermanentError(
                SpecError::UnknownOperation("a".to_string()),
            ) as ServiceError);
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
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SCHEMA, &config).unwrap());

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(QueryParsingCacheLayer::new(NonZeroUsize::new(10).unwrap()))
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
