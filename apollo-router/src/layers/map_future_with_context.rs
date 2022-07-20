//! Extension of map_future layer. Allows mapping of the future using some information obtained from the request.
//!
//! See [`Layer`] and [`Service`] for more details.
//!

use std::future::Future;
use std::task::Context;
use std::task::Poll;

use tower::Layer;
use tower::Service;

#[derive(Clone)]
pub struct MapFutureWithContextLayer<C, F> {
    ctx_fn: C,
    map_fn: F,
}

impl<C, F> MapFutureWithContextLayer<C, F> {
    pub fn new(ctx_fn: C, map_fn: F) -> Self {
        Self { ctx_fn, map_fn }
    }
}

impl<S, C, F> Layer<S> for MapFutureWithContextLayer<C, F>
where
    F: Clone,
    C: Clone,
{
    type Service = MapFutureWithContextService<S, C, F>;

    fn layer(&self, inner: S) -> Self::Service {
        MapFutureWithContextService::new(inner, self.ctx_fn.clone(), self.map_fn.clone())
    }
}

pub struct MapFutureWithContextService<S, C, F> {
    inner: S,
    ctx_fn: C,
    map_fn: F,
}

impl<S, C, F> MapFutureWithContextService<S, C, F> {
    pub fn new(inner: S, ctx_fn: C, map_fn: F) -> MapFutureWithContextService<S, C, F>
    where
        C: Clone,
        F: Clone,
    {
        MapFutureWithContextService {
            inner,
            ctx_fn,
            map_fn,
        }
    }
}

impl<R, S, F, C, T, E, Fut, Ctx> Service<R> for MapFutureWithContextService<S, C, F>
where
    S: Service<R>,
    C: FnMut(&R) -> Ctx,
    F: FnMut(Ctx, S::Future) -> Fut,
    E: From<S::Error>,
    Fut: Future<Output = Result<T, E>>,
{
    type Response = T;
    type Error = E;
    type Future = Fut;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(From::from)
    }

    fn call(&mut self, req: R) -> Self::Future {
        let ctx = (self.ctx_fn)(&req);
        (self.map_fn)(ctx, self.inner.call(req))
    }
}

#[cfg(test)]
mod test {
    use http::HeaderValue;
    use tower::BoxError;
    use tower::Service;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use crate::layers::ServiceBuilderExt;
    use crate::plugin::test::MockRouterService;
    use crate::RouterRequest;
    use crate::RouterResponse;

    #[tokio::test]
    async fn test_layer() -> Result<(), BoxError> {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            .once()
            .returning(|_| Ok(RouterResponse::fake_builder().build().unwrap()));

        let mut service = ServiceBuilder::new()
            .map_future_with_context(
                |req: &RouterRequest| {
                    req.originating_request
                        .headers()
                        .get("hello")
                        .cloned()
                        .unwrap()
                },
                |ctx: HeaderValue, resp| async move {
                    let resp: Result<RouterResponse, BoxError> = resp.await;
                    resp.map(|mut response| {
                        response.response.headers_mut().insert("hello", ctx.clone());
                        response
                    })
                },
            )
            .service(mock_service.build());

        let result = service
            .ready()
            .await
            .unwrap()
            .call(
                RouterRequest::fake_builder()
                    .header("hello", "world")
                    .build()
                    .unwrap(),
            )
            .await?;
        assert_eq!(
            result.response.headers().get("hello"),
            Some(&HeaderValue::from_static("world"))
        );
        Ok(())
    }
}
