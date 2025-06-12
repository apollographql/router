//! Tower layers wrapping the mis-named PersistedQueryLayer implementation into actual tower
//! layers.
//!
//! Long-term, we should move the actual implementation code into a service structure, but that is
//! more work especially to translate the tests.

use super::PersistedQueryLayer;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use std::sync::Arc;
use tower::BoxError;
use tower::Service;

pub(crate) struct ExpandIdsLayer {
    wrapped: Arc<PersistedQueryLayer>,
}

impl ExpandIdsLayer {
    pub(crate) fn new(wrapped: Arc<PersistedQueryLayer>) -> Self {
        Self { wrapped }
    }
}

impl<S> tower::Layer<S> for ExpandIdsLayer {
    type Service = ExpandIdsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ExpandIdsService {
            inner,
            wrapped: self.wrapped.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExpandIdsService<S> {
    inner: S,
    wrapped: Arc<PersistedQueryLayer>,
}

impl<S> Service<SupergraphRequest> for ExpandIdsService<S>
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = SupergraphResponse;
    type Error = BoxError;
    // XXX(@goto-bus-stop): some reusable response future type that is either a call to an inner
    // service, or a short-circuited response would be useful
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: SupergraphRequest) -> Self::Future {
        match self.wrapped.supergraph_request(req) {
            Ok(req) => self.inner.call(req).boxed(),
            Err(res) => std::future::ready(Ok(res)).boxed(),
        }
    }
}

pub(crate) struct EnforceSafelistLayer {
    wrapped: Arc<PersistedQueryLayer>,
}

impl EnforceSafelistLayer {
    pub(crate) fn new(wrapped: Arc<PersistedQueryLayer>) -> Self {
        Self { wrapped }
    }
}

impl<S> tower::Layer<S> for EnforceSafelistLayer {
    type Service = EnforceSafelistService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        EnforceSafelistService {
            inner,
            wrapped: self.wrapped.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct EnforceSafelistService<S> {
    inner: S,
    wrapped: Arc<PersistedQueryLayer>,
}

impl<S> Service<SupergraphRequest> for EnforceSafelistService<S>
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = SupergraphResponse;
    type Error = BoxError;
    // XXX(@goto-bus-stop): some reusable response future type that is either a call to an inner
    // service, or a short-circuited response would be useful
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
            match wrapped.supergraph_request_with_analyzed_query(req).await {
                Ok(req) => inner.call(req).await,
                Err(res) => Ok(res),
            }
        })
    }
}
