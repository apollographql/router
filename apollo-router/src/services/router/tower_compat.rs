//! Tower layers wrapping old non-tower layer-like things into real tower layers.
//!
//! Long-term, we should move the actual implementation code into a service structure, but that is
//! more work especially to translate the tests.

use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::layers::apq::APQLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use futures::future::BoxFuture;
use std::sync::Arc;
use tower::BoxError;
use tower::Service;

pub(crate) struct APQCachingLayer {
    wrapped: Arc<APQLayer>,
}

impl APQCachingLayer {
    pub(crate) fn new(wrapped: Arc<APQLayer>) -> Self {
        Self { wrapped }
    }
}

impl<S> tower::Layer<S> for APQCachingLayer {
    type Service = APQCachingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        APQCachingService {
            inner,
            wrapped: self.wrapped.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct APQCachingService<S> {
    inner: S,
    wrapped: Arc<APQLayer>,
}

impl<S> Service<SupergraphRequest> for APQCachingService<S>
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = SupergraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: SupergraphRequest) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        let wrapped = self.wrapped.clone();
        Box::pin(async move {
            match wrapped.supergraph_request(req).await {
                Ok(req) => inner.call(req).await,
                Err(res) => Ok(res),
            }
        })
    }
}

pub(crate) struct ParseQueryLayer {
    wrapped: Arc<QueryAnalysisLayer>,
}

impl ParseQueryLayer {
    pub(crate) fn new(wrapped: Arc<QueryAnalysisLayer>) -> Self {
        Self { wrapped }
    }
}

impl<S> tower::Layer<S> for ParseQueryLayer {
    type Service = ParseQueryService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ParseQueryService {
            inner,
            wrapped: self.wrapped.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ParseQueryService<S> {
    inner: S,
    wrapped: Arc<QueryAnalysisLayer>,
}

impl<S> Service<SupergraphRequest> for ParseQueryService<S>
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = SupergraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: SupergraphRequest) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        let wrapped = self.wrapped.clone();
        Box::pin(async move {
            match wrapped.supergraph_request(req).await {
                Ok(req) => inner.call(req).await,
                Err(res) => Ok(res),
            }
        })
    }
}
