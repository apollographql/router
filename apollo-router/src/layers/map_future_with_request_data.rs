//! Extension of map_future layer. Allows mapping of the future using information obtained from the request.
//!
//! See [`Layer`] and [`Service`] for more details.

use std::future::Future;
use std::task::Context;
use std::task::Poll;

use tower::Layer;
use tower::Service;

/// [`Layer`] for mapping futures with request data. See [`ServiceBuilderExt::map_future_with_request_data()`](crate::layers::ServiceBuilderExt::map_future_with_request_data()).
#[derive(Clone)]
pub struct MapFutureWithRequestDataLayer<RF, MF> {
    req_fn: RF,
    map_fn: MF,
}

impl<RF, MF> MapFutureWithRequestDataLayer<RF, MF> {
    /// Create a new instance.
    pub fn new(req_fn: RF, map_fn: MF) -> Self {
        Self { req_fn, map_fn }
    }
}

impl<S, RF, MF> Layer<S> for MapFutureWithRequestDataLayer<RF, MF>
where
    RF: Clone,
    MF: Clone,
{
    type Service = MapFutureWithRequestDataService<S, RF, MF>;

    fn layer(&self, inner: S) -> Self::Service {
        MapFutureWithRequestDataService::new(inner, self.req_fn.clone(), self.map_fn.clone())
    }
}

/// [`Service`] for mapping futures with request data. See [`ServiceBuilderExt::map_future_with_request_data()`](crate::layers::ServiceBuilderExt::map_future_with_request_data()).
#[derive(Clone)]
pub struct MapFutureWithRequestDataService<S, RF, MF> {
    inner: S,
    req_fn: RF,
    map_fn: MF,
}

impl<S, RF, MF> MapFutureWithRequestDataService<S, RF, MF> {
    /// Create a new instance.
    pub fn new(inner: S, req_fn: RF, map_fn: MF) -> MapFutureWithRequestDataService<S, RF, MF>
    where
        RF: Clone,
        MF: Clone,
    {
        MapFutureWithRequestDataService {
            inner,
            req_fn,
            map_fn,
        }
    }
}

impl<R, S, MF, RF, T, E, Fut, ReqData> Service<R> for MapFutureWithRequestDataService<S, RF, MF>
where
    S: Service<R>,
    RF: FnMut(&R) -> ReqData,
    MF: FnMut(ReqData, S::Future) -> Fut,
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
        let data = (self.req_fn)(&req);
        (self.map_fn)(data, self.inner.call(req))
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
    use crate::plugin::test::MockSupergraphService;
    use crate::services::SupergraphRequest;
    use crate::services::SupergraphResponse;

    #[tokio::test]
    async fn test_layer() -> Result<(), BoxError> {
        let mut mock_service = MockSupergraphService::new();
        mock_service
            .expect_call()
            .once()
            .returning(|_| Ok(SupergraphResponse::fake_builder().build().unwrap()));

        let mut service = ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &SupergraphRequest| {
                    req.supergraph_request
                        .headers()
                        .get("hello")
                        .cloned()
                        .unwrap()
                },
                |value: HeaderValue, resp| async move {
                    let resp: Result<SupergraphResponse, BoxError> = resp.await;
                    resp.map(|mut response| {
                        response
                            .response
                            .headers_mut()
                            .insert("hello", value.clone());
                        response
                    })
                },
            )
            .service(mock_service);

        let result = service
            .ready()
            .await
            .unwrap()
            .call(
                SupergraphRequest::fake_builder()
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
