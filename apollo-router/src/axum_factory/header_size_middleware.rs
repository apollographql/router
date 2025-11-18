//! Middleware to enforce HTTP header size limits
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use axum::response::Response;
use bytesize::ByteSize;
use futures::ready;
use http::Request;
use http::StatusCode;
use pin_project_lite::pin_project;
use tower::Layer;
use tower::Service;

/// Layer that enforces maximum header size limits
#[derive(Clone)]
pub(super) struct HeaderSizeLimitLayer {
    max_header_size: Option<ByteSize>,
}

impl HeaderSizeLimitLayer {
    pub(super) fn new(max_header_size: Option<ByteSize>) -> Self {
        Self { max_header_size }
    }
}

impl<S> Layer<S> for HeaderSizeLimitLayer {
    type Service = HeaderSizeLimitService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HeaderSizeLimitService {
            inner: service,
            max_header_size: self.max_header_size,
        }
    }
}

/// Service that enforces maximum header size limits
#[derive(Clone)]
pub(super) struct HeaderSizeLimitService<S> {
    inner: S,
    max_header_size: Option<ByteSize>,
}

impl<S, ReqBody> Service<Request<ReqBody>> for HeaderSizeLimitService<S>
where
    S: Service<Request<ReqBody>, Response = Response, Error = Infallible>,
{
    type Response = Response;
    type Error = Infallible;
    type Future = HeaderSizeLimitFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        // Check header sizes if limit is configured
        if let Some(max_size) = self.max_header_size {
            let max_size_bytes = max_size.as_u64() as usize;

            for (name, value) in request.headers() {
                let header_size = name.as_str().len() + value.len();
                if header_size > max_size_bytes {
                    tracing::debug!(
                        header_name = %name,
                        header_size = header_size,
                        max_size = max_size_bytes,
                        "Header size exceeds limit"
                    );

                    // Return 431 Request Header Fields Too Large
                    let response = Response::builder()
                        .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
                        .body(axum::body::Body::from("Request header field too large"))
                        .unwrap();

                    return HeaderSizeLimitFuture {
                        kind: HeaderSizeLimitFutureKind::Error {
                            response: Some(response),
                        },
                    };
                }
            }
        }

        // Headers are within limit, proceed with request
        HeaderSizeLimitFuture {
            kind: HeaderSizeLimitFutureKind::Service {
                future: self.inner.call(request),
            },
        }
    }
}

pin_project! {
    /// Future for header size limit enforcement
    pub(super) struct HeaderSizeLimitFuture<F> {
        #[pin]
        kind: HeaderSizeLimitFutureKind<F>,
    }
}

pin_project! {
    #[project = HeaderSizeLimitFutureKindProj]
    enum HeaderSizeLimitFutureKind<F> {
        Service {
            #[pin]
            future: F,
        },
        Error {
            response: Option<Response>,
        },
    }
}

impl<F> Future for HeaderSizeLimitFuture<F>
where
    F: Future<Output = Result<Response, Infallible>>,
{
    type Output = Result<Response, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        match this.kind.project() {
            HeaderSizeLimitFutureKindProj::Service { future } => {
                let response = ready!(future.poll(cx))?;
                Poll::Ready(Ok(response))
            }
            HeaderSizeLimitFutureKindProj::Error { response } => {
                let response = response.take().expect("polled after ready");
                Poll::Ready(Ok(response))
            }
        }
    }
}
