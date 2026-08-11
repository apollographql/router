//! Enforces the configured subgraph response size limit on the HTTP client service.
//!
//! Rather than eagerly buffering the response body to check its size, this replaces the body
//! with an `http_body_util::Limited` wrapper, so the limit is enforced lazily as the body is
//! read further up the stack.

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

/// Extension type placed on the request context to signal the subgraph response size limit.
#[derive(Clone, Copy, Debug, Ord, PartialOrd, PartialEq, Eq)]
pub(crate) struct SubgraphResponseSizeLimit(pub usize);

/// Error type returned by the response size limiting Body type when more than the maximum amount of
/// bytes have been read.
#[derive(Debug, thiserror::Error)]
#[error("subgraph response size limit exceeded")]
pub(crate) struct ResponseSizeLimitError;

/// Limit the response body size.
///
/// Replaces the response body with a type that errors with a [`ResponseSizeLimitError`] once more
/// bytes than the limit have been read.
///
/// # Context
/// Reads:
/// - [`SubgraphResponseSizeLimit`]
///
/// # Metrics
/// Emits `apollo.router.limits.subgraph_response_size.exceeded` when the limit is hit.
#[derive(Clone)]
pub(crate) struct SubgraphResponseSizeLimitLayer {
    subgraph_name: Arc<str>,
}

impl SubgraphResponseSizeLimitLayer {
    pub(crate) fn new(subgraph_name: impl Into<Arc<str>>) -> Self {
        Self {
            subgraph_name: subgraph_name.into(),
        }
    }
}

impl<S> Layer<S> for SubgraphResponseSizeLimitLayer {
    type Service = SubgraphResponseSizeLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SubgraphResponseSizeLimitService {
            inner,
            subgraph_name: self.subgraph_name.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubgraphResponseSizeLimitService<S> {
    inner: S,
    subgraph_name: Arc<str>,
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
        let limit = req
            .context
            .extensions()
            .with_lock(|e| e.get::<SubgraphResponseSizeLimit>().copied());
        let fut = self.inner.call(req);
        let subgraph_name = self.subgraph_name.clone();

        Box::pin(async move {
            let mut response = fut.await?;
            if let Some(SubgraphResponseSizeLimit(limit)) = limit {
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
                        axum::Error::new(ResponseSizeLimitError)
                    } else {
                        axum::Error::new(err)
                    }
                });
                response.http_response =
                    http::Response::from_parts(parts, RouterBody::new(limited));
            }
            Ok(response)
        })
    }
}
