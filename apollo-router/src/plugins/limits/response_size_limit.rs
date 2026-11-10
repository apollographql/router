//! A layer enforcing configured subgraph response size limits on the HTTP client service.

use std::sync::Arc;

use futures::future::BoxFuture;
use http_body_util::BodyExt as _;
use http_body_util::LengthLimitError;
use http_body_util::Limited;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router::body::RouterBody;

/// A subgraph's response size limit in bytes, read from the limits plugin's configuration when the
/// subgraph's stack is built.
#[derive(Clone, Copy, Debug, Ord, PartialOrd, PartialEq, Eq)]
pub(crate) struct SubgraphResponseSizeLimit(pub usize);

/// Error type returned by the response size limiting Body type when more than the maximum amount of
/// bytes have been read.
#[derive(Debug, thiserror::Error)]
#[error("subgraph response body exceeded limit of {limit} bytes")]
pub(crate) struct ResponseSizeLimitError {
    limit: usize,
}

/// Limit the response body size of one subgraph's responses to its configured limit. The subgraph
/// stage places it on the subgraph's HTTP client stack where a limit is configured.
///
/// Replaces the response body with a type that errors with a [`ResponseSizeLimitError`] once more
/// bytes than the limit have been read. The limit is fixed when the stack is built, so each
/// request reads its own subgraph's limit, whatever other subgraph requests of the same client
/// request are in flight.
///
/// # Metrics
/// Emits `apollo.router.limits.subgraph_response_size.exceeded` when the limit is hit.
#[derive(Clone)]
pub(crate) struct SubgraphResponseSizeLimitLayer {
    subgraph_name: Arc<str>,
    limit: usize,
}

impl SubgraphResponseSizeLimitLayer {
    pub(crate) fn new(subgraph_name: impl Into<Arc<str>>, limit: usize) -> Self {
        Self {
            subgraph_name: subgraph_name.into(),
            limit,
        }
    }
}

impl<S> Layer<S> for SubgraphResponseSizeLimitLayer {
    type Service = SubgraphResponseSizeLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SubgraphResponseSizeLimitService {
            inner,
            subgraph_name: self.subgraph_name.clone(),
            limit: self.limit,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubgraphResponseSizeLimitService<S> {
    inner: S,
    subgraph_name: Arc<str>,
    limit: usize,
}

impl<S> Service<HttpRequest> for SubgraphResponseSizeLimitService<S>
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        let fut = self.inner.call(req);
        let subgraph_name = self.subgraph_name.clone();
        let limit = self.limit;

        Box::pin(async move {
            let mut response = fut.await?;
            let (parts, body) = response.http_response.into_parts();
            let limited = Limited::new(body, limit).map_err(move |err| {
                if err.downcast_ref::<LengthLimitError>().is_some() {
                    u64_counter!(
                        "apollo.router.limits.subgraph_response_size.exceeded",
                        "Number of subgraph responses aborted because they exceeded the configured response size limit",
                        1,
                        subgraph.name = subgraph_name.to_string()
                    );
                    // Return our own type instead of the upstream LengthLimitError, to report a
                    // custom error message.
                    axum::Error::new(ResponseSizeLimitError { limit })
                } else {
                    axum::Error::new(err)
                }
            });
            response.http_response = http::Response::from_parts(parts, RouterBody::new(limited));
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Context;
    use crate::metrics::FutureMetricsExt as _;
    use crate::services::router;

    const SUBGRAPH_NAME: &str = "test-subgraph";

    fn request() -> HttpRequest {
        HttpRequest {
            http_request: http::Request::builder()
                .body(router::body::empty())
                .unwrap(),
            context: Context::new(),
        }
    }

    fn response_of_size(size: usize) -> HttpResponse {
        HttpResponse {
            http_response: http::Response::builder()
                .body(router::body::from_bytes(vec![0; size]))
                .unwrap(),
            context: Context::new(),
        }
    }

    #[tokio::test]
    async fn test_under_limit() {
        const RESPONSE_SIZE: usize = 1_000;
        const LIMIT: usize = 10_000;

        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_response(response_of_size(RESPONSE_SIZE));
        });

        let service = ServiceBuilder::new()
            .layer(SubgraphResponseSizeLimitLayer::new(SUBGRAPH_NAME, LIMIT))
            .service(mock);

        let response = service.oneshot(request()).await.unwrap();

        let _bytes = response
            .http_response
            .into_body()
            .collect()
            .await
            .expect("small response should succeed");

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_over_limit() {
        async {
            const RESPONSE_SIZE: usize = 10_000;
            const LIMIT: usize = 1_000;

            let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
            let driver = tokio::spawn(async move {
                let (_req, responder) = handle.next_request().await.unwrap();
                responder.send_response(response_of_size(RESPONSE_SIZE));
            });

            let service = ServiceBuilder::new()
                .layer(SubgraphResponseSizeLimitLayer::new(SUBGRAPH_NAME, LIMIT))
                .service(mock);

            let response = service.oneshot(request()).await.unwrap();

            let result = response.http_response.into_body().collect().await;

            let Err(err) = result else {
                panic!("response size should have been limited");
            };
            assert_eq!(
                err.to_string(),
                "subgraph response body exceeded limit of 1000 bytes"
            );

            crate::plugin::test::await_mock_driver(driver).await;

            assert_counter!(
                "apollo.router.limits.subgraph_response_size.exceeded",
                1,
                "subgraph.name" = SUBGRAPH_NAME.to_string()
            );
        }
        .with_metrics()
        .await;
    }
}
