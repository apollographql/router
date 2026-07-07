//! Tower layer that enforces license limits on the main router pipeline.
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use axum::body::Body;
use axum::body::Bytes;
use axum::response::IntoResponse;
use axum::response::Response;
use futures::future::BoxFuture;
use http::Request;
use http::StatusCode;
use tokio::time::Instant;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::uplink::license_enforcement::APOLLO_ROUTER_LICENSE_EXPIRED;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;
use crate::uplink::license_enforcement::LicenseState;

#[derive(Clone)]
pub(crate) struct LicenseLayer {
    license: Arc<LicenseState>,
    start: Instant,
    delta: Arc<AtomicU64>,
}

impl LicenseLayer {
    pub(crate) fn new(license: Arc<LicenseState>) -> Self {
        Self {
            license,
            start: Instant::now(),
            delta: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<S> Layer<S> for LicenseLayer {
    type Service = LicenseService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LicenseService {
            inner,
            license: self.license.clone(),
            start: self.start,
            delta: self.delta.clone(),
        }
    }
}

/// Logs [`APOLLO_ROUTER_LICENSE_EXPIRED`] at most once a second while the license is
/// in a `LicensedHalt` or `LicensedWarn` state.
///
/// `start` and `delta` together track rate limiting: `delta` stores the seconds-since-`start`
/// at which we last logged, and we only log again once the current elapsed time has moved past
/// it — using `fetch_max` so that if multiple requests race to log for the same second, only the
/// one that actually advances the watermark logs. The ordering only needs to be `Relaxed` since
/// `delta` isn't guarding access to any other data.
fn log_license_expired_rate_limited(license: &LicenseState, start: Instant, delta: &AtomicU64) {
    if !matches!(
        license,
        LicenseState::LicensedHalt { limits: _ } | LicenseState::LicensedWarn { limits: _ }
    ) {
        return;
    }

    let elapsed_seconds = start.elapsed().as_secs();
    let last_elapsed_seconds = delta.fetch_max(elapsed_seconds, Ordering::Relaxed);
    if elapsed_seconds > last_elapsed_seconds {
        ::tracing::error!(
            code = APOLLO_ROUTER_LICENSE_EXPIRED,
            LICENSE_EXPIRED_SHORT_MESSAGE
        );
    }
}

#[derive(Clone)]
pub(crate) struct LicenseService<S> {
    inner: S,
    license: Arc<LicenseState>,
    start: Instant,
    delta: Arc<AtomicU64>,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for LicenseService<S>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: http_body::Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<BoxError>,
{
    type Response = Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        let license = self.license.clone();
        let start = self.start;
        let delta = self.delta.clone();
        let service = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, service);

        Box::pin(async move {
            log_license_expired_rate_limited(&license, start, &delta);

            if matches!(&*license, LicenseState::LicensedHalt { limits: _ }) {
                return Ok(http::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::default())
                    .expect("canned response must be valid")
                    .into_response());
            }

            let response = inner.call(request).await?;
            Ok(response.map(Body::new))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tower::Service;
    use tower::ServiceExt;
    use tower_test::mock;

    use super::*;

    fn request() -> Request<Body> {
        Request::builder().body(Body::empty()).unwrap()
    }

    fn ok_response() -> Response {
        http::Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("inner response"))
            .unwrap()
    }

    // The mock's `Error` is `tower_test::mock::error::Error`, but `LicenseService` requires an
    // `Infallible` inner error to match what the rest of the axum stack guarantees; the mock
    // never sends an error in these tests, so the conversion is unreachable.
    fn mock_never_errors(_: BoxError) -> BoxError {
        unreachable!("test mock never sends an error")
    }

    type MockedInner =
        tower::util::MapErr<mock::Mock<Request<Body>, Response>, fn(BoxError) -> BoxError>;

    fn mocked_service(
        license: LicenseState,
    ) -> (
        LicenseService<MockedInner>,
        mock::Handle<Request<Body>, Response>,
    ) {
        let (mock_service, handle) = mock::pair::<Request<Body>, Response>();
        let inner = mock_service.map_err(mock_never_errors as fn(BoxError) -> BoxError);
        (LicenseLayer::new(Arc::new(license)).layer(inner), handle)
    }

    #[tokio::test]
    async fn licensed_requests_reach_the_inner_service() {
        let (service, mut handle) = mocked_service(LicenseState::Licensed { limits: None });
        handle.allow(1);
        let driver = tokio::spawn(async move {
            let (_request, respond) = handle
                .next_request()
                .await
                .expect("service should be called");
            respond.send_response(ok_response());
        });

        let response = service.oneshot(request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn licensed_warn_requests_still_reach_the_inner_service() {
        let (service, mut handle) = mocked_service(LicenseState::LicensedWarn { limits: None });
        handle.allow(1);
        let driver = tokio::spawn(async move {
            let (_request, respond) = handle
                .next_request()
                .await
                .expect("service should be called");
            respond.send_response(ok_response());
        });

        let response = service.oneshot(request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn licensed_halt_requests_short_circuit_with_a_500_and_never_reach_the_inner_service() {
        let (mut service, handle) = mocked_service(LicenseState::LicensedHalt { limits: None });

        let response = service.call(request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn logs_license_expired_once_then_rate_limits_further_logs_within_the_same_second() {
        let _guard = crate::test_harness::tracing_test::dispatcher_guard();
        let (mut service, handle) = mocked_service(LicenseState::LicensedHalt { limits: None });
        tokio::time::advance(Duration::from_secs(2)).await;

        service.call(request()).await.unwrap();
        service.call(request()).await.unwrap();

        crate::test_harness::tracing_test::logs_assert(|lines| {
            let count = lines
                .iter()
                .filter(|line| line.contains(LICENSE_EXPIRED_SHORT_MESSAGE))
                .count();
            if count == 1 {
                Ok(())
            } else {
                Err(format!(
                    "expected exactly one rate-limited log line, found {count}"
                ))
            }
        })
        .unwrap();
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }
}
