//! The Router's own enforcement of `limits.router.http2_max_headers_list_bytes`.
//!
//! When h2 rejects an oversized header list itself, it either answers with a bare 431 or closes
//! the connection, and the request never reaches the Router's telemetry. So the Router advertises
//! a larger header-list size to h2 (see [`advertised_size`]) and rejects requests over the
//! configured limit itself, which records the 431 like any other response. Graph requests get the
//! check from [`RouterHeaderListLimitLayer`] in the router service stack; plugin endpoints, which
//! don't go through that stack, get it from [`EndpointHeaderListLimitLayer`].
//! The check applies only to HTTP/2: HTTP/1.1 keeps hyper's count and buffer limits.

use std::future::Ready;
use std::task::Context;
use std::task::Poll;

use bytesize::ByteSize;
use futures::future::Either;
use http::StatusCode;
use http::Version;
use tower::Layer;
use tower::Service;

use super::layer::RequestSizeLimitError;
use crate::services::router;

/// How far above the configured limit h2 may accept a header list, at most.
///
/// Up to 3 × the limit is accepted, so h2's own rejections (its bare 431 and its connection
/// close at a multiple of the advertised size) stay well clear of the Router's 431. The cap
/// bounds the extra header memory: h2 can hold one header list per open stream (200 by
/// default), so the cap adds at most 200 × 256 KiB = 50 MiB per connection.
const MAX_HEADROOM: u64 = ByteSize::kib(256).as_u64();

/// The `SETTINGS_MAX_HEADER_LIST_SIZE` to give h2 for a configured `limit`:
/// `limit + min(3 × limit, 256 KiB)`, saturating at `u32::MAX` because HTTP/2 settings are 32-bit.
pub(crate) fn advertised_size(limit: ByteSize) -> u32 {
    let limit = limit.as_u64();
    let headroom = limit.saturating_mul(3).min(MAX_HEADROOM);
    u32::try_from(limit.saturating_add(headroom)).unwrap_or(u32::MAX)
}

/// Whether `request` is an HTTP/2 request whose header list is larger than `limit`.
fn exceeds_limit<B>(request: &http::Request<B>, limit: ByteSize) -> bool {
    request.version() == Version::HTTP_2 && header_list_size(request) > limit.as_u64()
}

/// Answers an HTTP/2 graph request whose header list is over the limit with a 431 GraphQL error,
/// in the same shape as the 413 and 414 size-limit errors.
#[derive(Clone)]
pub(crate) struct RouterHeaderListLimitLayer {
    limit: ByteSize,
}

impl RouterHeaderListLimitLayer {
    pub(crate) fn new(limit: ByteSize) -> Self {
        Self { limit }
    }
}

impl<S> Layer<S> for RouterHeaderListLimitLayer {
    type Service = RouterHeaderListLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RouterHeaderListLimit {
            inner,
            limit: self.limit,
        }
    }
}

/// The service [`RouterHeaderListLimitLayer`] wraps around the router service.
#[derive(Clone)]
pub(crate) struct RouterHeaderListLimit<S> {
    inner: S,
    limit: ByteSize,
}

impl<S> Service<router::Request> for RouterHeaderListLimit<S>
where
    S: Service<router::Request, Response = router::Response>,
{
    type Response = router::Response;
    type Error = S::Error;
    type Future = Either<Ready<Result<router::Response, S::Error>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: router::Request) -> Self::Future {
        if exceeds_limit(&request.router_request, self.limit) {
            let response =
                RequestSizeLimitError::OversizedHeaderList.into_response(request.context);
            Either::Left(std::future::ready(Ok(response)))
        } else {
            Either::Right(self.inner.call(request))
        }
    }
}

/// Answers an HTTP/2 request whose header list is over the limit with a bare 431, for plugin
/// endpoints. Without it, they would accept header lists up to the advertised size instead of the
/// configured limit.
#[derive(Clone)]
pub(crate) struct EndpointHeaderListLimitLayer {
    limit: ByteSize,
}

impl EndpointHeaderListLimitLayer {
    pub(crate) fn new(limit: ByteSize) -> Self {
        Self { limit }
    }
}

impl<S> Layer<S> for EndpointHeaderListLimitLayer {
    type Service = EndpointHeaderListLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        EndpointHeaderListLimit {
            inner,
            limit: self.limit,
        }
    }
}

/// The service [`EndpointHeaderListLimitLayer`] wraps around an endpoint.
#[derive(Clone)]
pub(crate) struct EndpointHeaderListLimit<S> {
    inner: S,
    limit: ByteSize,
}

impl<S, ReqBody, ResBody> Service<http::Request<ReqBody>> for EndpointHeaderListLimit<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    ResBody: Default,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Either<Ready<Result<S::Response, S::Error>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<ReqBody>) -> Self::Future {
        if exceeds_limit(&request, self.limit) {
            let mut response = http::Response::new(ResBody::default());
            *response.status_mut() = StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE;
            Either::Left(std::future::ready(Ok(response)))
        } else {
            Either::Right(self.inner.call(request))
        }
    }
}

/// The header-list size as HTTP/2 defines it (RFC 9113, section 6.5.2): name length + value
/// length + 32 bytes for every field, pseudo-headers included.
///
/// h2 has already turned the pseudo-headers into the method and URI, so they are rebuilt here.
/// Layers ahead of this check can only remove bytes (a trimmed trailing slash, for example), so
/// the result is never larger than the size h2 measured.
fn header_list_size<B>(request: &http::Request<B>) -> u64 {
    fn field(name: &str, value_len: usize) -> u64 {
        (name.len() + value_len + 32) as u64
    }

    let uri = request.uri();
    let pseudo_headers = field(":method", request.method().as_str().len())
        + uri.scheme_str().map_or(0, |s| field(":scheme", s.len()))
        + uri
            .authority()
            .map_or(0, |a| field(":authority", a.as_str().len()))
        + uri
            .path_and_query()
            .map_or(0, |p| field(":path", p.as_str().len()));
    let headers: u64 = request
        .headers()
        .iter()
        .map(|(name, value)| field(name.as_str(), value.len()))
        .sum();

    pseudo_headers + headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::default_limit(ByteSize::kib(16), 64 * 1024)]
    #[case::headroom_reaches_cap(ByteSize::b(256 * 1024 / 3 + 1), 256 * 1024 / 3 + 1 + 256 * 1024)]
    #[case::one_mib(ByteSize::mib(1), 1024 * 1024 + 256 * 1024)]
    #[case::twenty_mib(ByteSize::mib(20), 20 * 1024 * 1024 + 256 * 1024)]
    #[case::saturates(ByteSize::b(u32::MAX as u64), u32::MAX)]
    fn advertises_limit_plus_bounded_headroom(#[case] limit: ByteSize, #[case] expected: u32) {
        assert_eq!(advertised_size(limit), expected);
    }

    fn http2_request() -> http::request::Builder {
        http::Request::builder()
            .version(Version::HTTP_2)
            .method("POST")
            .uri("http://localhost:4000/graphql")
    }

    /// Pseudo-headers of `http2_request()`: :method POST, :scheme http,
    /// :authority localhost:4000, :path /graphql.
    const PSEUDO_HEADERS_SIZE: u64 = (7 + 4 + 32) + (7 + 4 + 32) + (10 + 14 + 32) + (5 + 8 + 32);

    #[test]
    fn counts_pseudo_headers_and_every_field() {
        let request = http2_request()
            .header("cookie", "a=1")
            .header("cookie", "b=22")
            .body(())
            .unwrap();
        let cookies = (6 + 3 + 32) + (6 + 4 + 32);
        assert_eq!(header_list_size(&request), PSEUDO_HEADERS_SIZE + cookies);
    }

    #[test]
    fn rejects_only_above_the_limit() {
        let value = "v".repeat(100);
        let request = http2_request().header("x-a", &value).body(()).unwrap();
        let size = PSEUDO_HEADERS_SIZE + (3 + 100 + 32);

        assert!(!exceeds_limit(&request, ByteSize::b(size)));
        assert!(exceeds_limit(&request, ByteSize::b(size - 1)));
    }

    #[test]
    fn ignores_http1_requests() {
        let request = http::Request::builder()
            .version(Version::HTTP_11)
            .uri("http://localhost:4000/graphql")
            .header("cookie", "v".repeat(100))
            .body(())
            .unwrap();
        assert!(!exceeds_limit(&request, ByteSize::b(1)));
    }

    mod layer {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::Ordering;

        use tower::ServiceExt;
        use tower::service_fn;

        use super::*;

        const LIMIT: ByteSize = ByteSize::b(PSEUDO_HEADERS_SIZE + 3 + 100 + 32);

        /// Sends `request` through the layer. Returns the response and whether the endpoint ran.
        async fn through_layer(request: http::Request<()>) -> (http::Response<String>, bool) {
            let called = Arc::new(AtomicBool::new(false));
            let endpoint = service_fn({
                let called = called.clone();
                move |_: http::Request<()>| {
                    called.store(true, Ordering::SeqCst);
                    async { Ok::<_, std::convert::Infallible>(http::Response::new("ok".into())) }
                }
            });
            let response = EndpointHeaderListLimitLayer::new(LIMIT)
                .layer(endpoint)
                .oneshot(request)
                .await
                .unwrap();
            (response, called.load(Ordering::SeqCst))
        }

        #[tokio::test]
        async fn rejects_http2_requests_over_the_limit_without_calling_the_endpoint() {
            let request = http2_request()
                .header("x-a", "v".repeat(101))
                .body(())
                .unwrap();
            let (response, called) = through_layer(request).await;
            assert_eq!(
                response.status(),
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
            );
            assert!(response.body().is_empty());
            assert!(!called);
        }

        #[tokio::test]
        async fn passes_http2_requests_at_the_limit() {
            let request = http2_request()
                .header("x-a", "v".repeat(100))
                .body(())
                .unwrap();
            let (response, called) = through_layer(request).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(called);
        }

        #[tokio::test]
        async fn passes_http1_requests_over_the_limit() {
            let request = http::Request::builder()
                .version(Version::HTTP_11)
                .uri("http://localhost:4000/graphql")
                .header("x-a", "v".repeat(1000))
                .body(())
                .unwrap();
            let (response, called) = through_layer(request).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(called);
        }
    }
}
