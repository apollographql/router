use std::sync::Arc;

use futures::future::BoxFuture;
use futures::future::FutureExt as _;
use futures::future::TryFutureExt as _;
use tower::BoxError;

pub(crate) mod http_client;
pub(crate) mod http_server;
pub(crate) mod json_client;
pub(crate) mod json_server;

/// Convert request and response types. Provide functions from `core_compat` submodules
/// for request and response transformation.
pub(crate) struct ConvertLayer<OuterReq, InnerReq, InnerRes, OuterRes> {
    request: Arc<dyn Fn(OuterReq) -> Result<InnerReq, BoxError> + Send + Sync + 'static>,
    response: Arc<dyn Fn(InnerRes) -> Result<OuterRes, BoxError> + Send + Sync + 'static>,
}

impl<OuterReq, InnerReq, InnerRes, OuterRes> ConvertLayer<OuterReq, InnerReq, InnerRes, OuterRes> {
    pub(crate) fn new(
        request: impl Fn(OuterReq) -> Result<InnerReq, BoxError> + Send + Sync + 'static,
        response: impl Fn(InnerRes) -> Result<OuterRes, BoxError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            request: Arc::new(request),
            response: Arc::new(response),
        }
    }
}

impl<S, OuterReq, InnerReq, InnerRes, OuterRes> tower::Layer<S>
    for ConvertLayer<OuterReq, InnerReq, InnerRes, OuterRes>
{
    type Service = ConvertService<S, OuterReq, InnerReq, InnerRes, OuterRes>;
    fn layer(&self, inner: S) -> Self::Service {
        ConvertService {
            inner,
            request: self.request.clone(),
            response: self.response.clone(),
        }
    }
}

pub(crate) struct ConvertService<S, OuterReq, InnerReq, InnerRes, OuterRes> {
    inner: S,
    request: Arc<dyn Fn(OuterReq) -> Result<InnerReq, BoxError> + Send + Sync + 'static>,
    response: Arc<dyn Fn(InnerRes) -> Result<OuterRes, BoxError> + Send + Sync + 'static>,
}

impl<S, OuterReq, InnerReq, InnerRes, OuterRes> Clone
    for ConvertService<S, OuterReq, InnerReq, InnerRes, OuterRes>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            request: self.request.clone(),
            response: self.response.clone(),
        }
    }
}

impl<S, OuterReq, InnerReq, InnerRes, OuterRes> tower::Service<OuterReq>
    for ConvertService<S, OuterReq, InnerReq, InnerRes, OuterRes>
where
    S: tower::Service<InnerReq, Response = InnerRes, Error = BoxError> + Send + 'static,
    S::Future: Send + 'static,
    OuterReq: Send + 'static,
    InnerReq: Send + 'static,
    InnerRes: Send + 'static,
    OuterRes: Send + 'static,
{
    type Response = OuterRes;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: OuterReq) -> Self::Future {
        let req = (self.request)(req);
        let transform_response = self.response.clone();

        match req {
            Ok(req) => self
                .inner
                .call(req)
                .and_then(|res| async move { transform_response(res) })
                .boxed(),
            Err(err) => std::future::ready(Err(err)).boxed(),
        }
    }
}
