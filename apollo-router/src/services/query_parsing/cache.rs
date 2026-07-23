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
