// With regards to ELv2 licensing, this entire file is license key functionality

use crate::cache::storage::RedisCacheStorage;
use crate::SupergraphRequest;
use crate::SupergraphResponse;
use futures::future::BoxFuture;
use tower::BoxError;
use tower::Layer;
use tower::Service;

/// [`Layer`] for live queries implementation
#[derive(Clone)]
pub(crate) struct LiveLayer {
    cache: RedisCacheStorage,
}

impl LiveLayer {
    pub(crate) async fn new(cache: RedisCacheStorage) -> Self {
        Self { cache }
    }

    /*pub(crate) async fn request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        handle_request(request).await
    }*/
}

impl<S> Layer<S> for LiveLayer
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as Service<SupergraphRequest>>::Future: Send + 'static,
{
    type Service = LiveService<S>;

    fn layer(&self, service: S) -> Self::Service {
        let cache = self.cache.clone();
        LiveService {
            cache,
            inner: service,
        }
    }
}

struct LiveService<S> {
    inner: S,
    cache: RedisCacheStorage,
}

impl<S> Service<SupergraphRequest> for LiveService<S>
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,

    <S as Service<SupergraphRequest>>::Future: std::marker::Send + 'static,
{
    type Response = SupergraphResponse;

    type Error = <S as Service<SupergraphRequest>>::Error;

    type Future = BoxFuture<'static, Result<SupergraphResponse, BoxError>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: SupergraphRequest) -> Self::Future {
        match req.supergraph_request.body_mut().cursor.take() {
            None => Box::pin(self.inner.call(req))
                as BoxFuture<'static, Result<SupergraphResponse, BoxError>>,
            Some(cursor) => {
                todo!()
            }
        }
    }
}
/*pub(crate) async fn handle_request(
    cache: &RedisCacheStorage,
    mut request: SupergraphRequest,
) -> Result<SupergraphRequest, SupergraphResponse> {
    let cursor = match request.cursor.take() {
        None => return Ok(request),
        Some(cursor) => cursor,
    };

    match cursor
    todo!()
}
*/
