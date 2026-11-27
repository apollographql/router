//! A layer enforcing configured subgraph and connector source response size limits on the HTTP
//! client service.

use std::sync::Arc;

use futures::future::BoxFuture;
use http_body_util::BodyExt as _;
use http_body_util::LengthLimitError;
use http_body_util::Limited;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::pipeline::HttpClientTarget;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router::body::RouterBody;

/// Error type returned by the response size limiting Body type when more than the maximum amount of
/// bytes have been read.
#[derive(Debug, thiserror::Error)]
#[error("{kind} response body exceeded limit of {limit} bytes")]
pub(crate) struct ResponseSizeLimitError {
    /// `subgraph` or `connector`.
    kind: &'static str,
    limit: usize,
}

impl ResponseSizeLimitError {
    /// The response size limit error in `error`'s chain of sources, present when the body was cut
    /// off at the limit rather than failing for another reason.
    pub(crate) fn find<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        std::iter::successors(Some(error), |error| error.source())
            .find_map(|error| error.downcast_ref::<Self>())
    }
}

/// Whose responses a [`ResponseSizeLimitLayer`] limits.
#[derive(Clone, Debug)]
enum LimitedTarget {
    Subgraph(Arc<str>),
    /// Keyed by `<subgraph name>.<source name>`.
    ConnectorSource(Arc<str>),
}

impl LimitedTarget {
    /// Counts a response cut off at `limit`, and returns the error that reports it.
    fn exceeded(&self, limit: usize) -> ResponseSizeLimitError {
        match self {
            Self::Subgraph(name) => {
                u64_counter!(
                    "apollo.router.limits.subgraph_response_size.exceeded",
                    "Number of subgraph responses aborted because they exceeded the configured response size limit",
                    1,
                    subgraph.name = name.to_string()
                );
                ResponseSizeLimitError {
                    kind: "subgraph",
                    limit,
                }
            }
            Self::ConnectorSource(source) => {
                u64_counter!(
                    "apollo.router.limits.connector_response_size.exceeded",
                    "Number of connector responses aborted because they exceeded the configured response size limit",
                    1,
                    "connector.source" = source.to_string()
                );
                ResponseSizeLimitError {
                    kind: "connector",
                    limit,
                }
            }
        }
    }
}

/// Limits the size of one subgraph's or connector source's response bodies.
///
/// Replaces the response body with a type that errors with a [`ResponseSizeLimitError`] once more
/// bytes than the limit have been read.
///
/// # Metrics
/// Emits `apollo.router.limits.subgraph_response_size.exceeded` or
/// `apollo.router.limits.connector_response_size.exceeded` when the limit is hit.
#[derive(Clone)]
pub(crate) struct ResponseSizeLimitLayer {
    target: LimitedTarget,
    limit: usize,
}

impl ResponseSizeLimitLayer {
    pub(crate) fn new(target: HttpClientTarget<'_>, limit: usize) -> Self {
        let target = match target {
            HttpClientTarget::Subgraph(name) => LimitedTarget::Subgraph(name.into()),
            HttpClientTarget::ConnectorSource(source) => {
                LimitedTarget::ConnectorSource(source.into())
            }
        };
        Self { target, limit }
    }
}

impl<S> Layer<S> for ResponseSizeLimitLayer {
    type Service = ResponseSizeLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ResponseSizeLimitService {
            inner,
            target: self.target.clone(),
            limit: self.limit,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ResponseSizeLimitService<S> {
    inner: S,
    target: LimitedTarget,
    limit: usize,
}

impl<S> Service<HttpRequest> for ResponseSizeLimitService<S>
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError>,
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
        let target = self.target.clone();
        let limit = self.limit;

        Box::pin(async move {
            let mut response = fut.await?;
            let (parts, body) = response.http_response.into_parts();
            let limited = Limited::new(body, limit).map_err(move |err| {
                if err.downcast_ref::<LengthLimitError>().is_some() {
                    // Return our own type instead of the upstream LengthLimitError, to report a
                    // custom error message.
                    axum::Error::new(target.exceeded(limit))
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
    const SOURCE_KEY: &str = "test-subgraph.api";

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

    /// Reads a 10,000-byte response through a 1,000-byte limit for `target`.
    async fn read_over_limit(target: HttpClientTarget<'_>) -> axum::Error {
        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_response(response_of_size(10_000));
        });

        let service = ServiceBuilder::new()
            .layer(ResponseSizeLimitLayer::new(target, 1_000))
            .service(mock);

        let response = service.oneshot(request()).await.unwrap();
        let result = response.http_response.into_body().collect().await;
        crate::plugin::test::await_mock_driver(driver).await;

        let Err(err) = result else {
            panic!("response size should have been limited");
        };
        err
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
            .layer(ResponseSizeLimitLayer::new(
                HttpClientTarget::Subgraph(SUBGRAPH_NAME),
                LIMIT,
            ))
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
            let err = read_over_limit(HttpClientTarget::Subgraph(SUBGRAPH_NAME)).await;
            assert_eq!(
                err.to_string(),
                "subgraph response body exceeded limit of 1000 bytes"
            );

            assert_counter!(
                "apollo.router.limits.subgraph_response_size.exceeded",
                1,
                "subgraph.name" = SUBGRAPH_NAME.to_string()
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_connector_source_over_limit() {
        async {
            let err = read_over_limit(HttpClientTarget::ConnectorSource(SOURCE_KEY)).await;
            assert_eq!(
                err.to_string(),
                "connector response body exceeded limit of 1000 bytes"
            );

            assert_counter!(
                "apollo.router.limits.connector_response_size.exceeded",
                1,
                "connector.source" = SOURCE_KEY.to_string()
            );
        }
        .with_metrics()
        .await;
    }
}
