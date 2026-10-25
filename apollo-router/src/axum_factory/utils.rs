//! Utilities used for [`super::AxumHttpServerFactory`]

use std::net::SocketAddr;
use std::sync::Arc;
use std::task::Poll;

use futures::FutureExt;
use futures::future::BoxFuture;
use http::StatusCode;
use opentelemetry::global;
use opentelemetry::trace::TraceContextExt;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::load_shed::error::Overloaded;
use tower_http::trace::MakeSpan;
use tracing::Span;

use crate::Context;
use crate::graphql;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::pipeline_bypass::record_bypassed_request;
use crate::plugins::telemetry::span_factory;
use crate::services::router;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;
use crate::uplink::license_enforcement::LicenseState;

#[derive(Clone, Default)]
pub(crate) struct PropagatingMakeSpan {
    pub(crate) license: Arc<LicenseState>,
}

impl<B> MakeSpan<B> for PropagatingMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> Span {
        // This method needs to be moved to the telemetry plugin once we have a hook for the http request.

        // Before we make the span we need to attach span info that may have come in from the request.
        let context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&opentelemetry_http::HeaderExtractor(request.headers()))
        });

        // If there was no span from the request then it will default to the NOOP span.
        // Attaching the NOOP span has the effect of preventing further tracing.
        let span = if context.span().span_context().is_valid()
            || context.span().span_context().trace_id() != opentelemetry::trace::TraceId::INVALID
        {
            // We have a valid remote span, attach it to the current thread before creating the root span.
            let _context_guard = context.attach();
            span_factory::create_router(request)
        } else {
            // No remote span, we can go ahead and create the span without context.
            span_factory::create_router(request)
        };
        if matches!(
            &*self.license,
            LicenseState::LicensedWarn { limits: _ } | LicenseState::LicensedHalt { limits: _ }
        ) {
            span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            span.record("apollo_router.license", LICENSE_EXPIRED_SHORT_MESSAGE);
        }

        span
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionInfo {
    pub(crate) peer_address: Option<SocketAddr>,
    pub(crate) server_address: Option<SocketAddr>,
}

pub(crate) type ConnectionRouterService =
    tower::util::BoxCloneSyncService<router::Request, router::Response, tower::BoxError>;

/// Wraps a router service so every connection can share one instance, cloning it per request.
///
/// Once [`DEFAULT_BUFFER_SIZE`] requests are in flight, further requests answer with a 503
/// rather than waiting without bound.
pub(crate) fn connection_router_service(
    service: router::BoxCloneService,
) -> ConnectionRouterService {
    connection_router_service_with_limit(service, DEFAULT_BUFFER_SIZE)
}

fn connection_router_service_with_limit(
    service: router::BoxCloneService,
    max_in_flight: usize,
) -> ConnectionRouterService {
    ConnectionRouterService::new(
        ServiceBuilder::new()
            .map_future_with_request_data(|req: &router::Request| req.context.clone(), shed_as_503)
            .service(SharedPipeline::new(service, max_in_flight)),
    )
}

/// Shares one router pipeline between every connection, running each request on its own task.
///
/// Each request clones the pipeline and calls it from the request's task. A [`tower::buffer`]
/// would instead make every request's synchronous `call` work run on its single worker task,
/// capping throughput at what one core can do however many the router has.
///
/// Once `max_in_flight` requests are running, `call` fails with [`Overloaded`] straight away.
#[derive(Clone)]
struct SharedPipeline {
    // Plugins build a `router::BoxCloneService`, which is not `Sync`, so sharing it needs a lock.
    // The lock is held only while cloning.
    pipeline: Arc<parking_lot::Mutex<router::BoxCloneService>>,
    in_flight: Arc<Semaphore>,
}

impl SharedPipeline {
    fn new(pipeline: router::BoxCloneService, max_in_flight: usize) -> Self {
        Self {
            pipeline: Arc::new(parking_lot::Mutex::new(pipeline)),
            in_flight: Arc::new(Semaphore::new(max_in_flight)),
        }
    }
}

impl tower::Service<router::Request> for SharedPipeline {
    type Response = router::Response;
    type Error = tower::BoxError;
    type Future = BoxFuture<'static, Result<router::Response, tower::BoxError>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        // hyper awaits `poll_ready` inside each request's future, so `Pending` here would stall
        // the request rather than push back on the connection. `call` decides whether to shed.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: router::Request) -> Self::Future {
        // `try_acquire` never waits, so tokio's cooperative budget cannot shed a request that
        // had a free slot.
        let Ok(permit) = self.in_flight.clone().try_acquire_owned() else {
            return std::future::ready(Err(Overloaded::new().into())).boxed();
        };
        let pipeline = self.pipeline.lock().clone();
        async move {
            let response = pipeline.oneshot(request).await;
            drop(permit);
            response
        }
        .boxed()
    }
}

/// Answers a shed request with a 503 so callers never handle [`Overloaded`] themselves.
///
/// Belongs directly outside the [`SharedPipeline`] whose errors it renders.
async fn shed_as_503(
    context: Context,
    future: impl Future<Output = Result<router::Response, tower::BoxError>>,
) -> Result<router::Response, tower::BoxError> {
    match future.await {
        Err(err) if err.is::<Overloaded>() => {
            // Shedding happens before the router pipeline, and its `http.server.*` instruments
            // with it, so record the duration here to keep shed requests visible.
            record_bypassed_request(
                StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                context.created_at.elapsed(),
            );

            // Debug rather than warn: shedding is per-request, so a sustained overload would
            // otherwise emit a line per rejected request while the router is already struggling.
            tracing::debug!(
                code = "REQUEST_CONCURRENCY_LIMITED",
                "the router is at its limit of in-flight requests, shedding request",
            );

            Ok(router::Response::error_builder()
                .status_code(StatusCode::SERVICE_UNAVAILABLE)
                .error(
                    graphql::Error::builder()
                        .message("Your request has been concurrency limited waiting for the router")
                        .extension_code("REQUEST_CONCURRENCY_LIMITED")
                        .build(),
                )
                .context(context)
                .build()
                .expect("overloaded response should build"))
        }
        other => other,
    }
}

/// Stands in for a saturated pipeline: `poll_ready` never resolves, so `call` is never reached.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct NeverReady;

#[cfg(test)]
impl tower::Service<router::Request> for NeverReady {
    type Response = router::Response;
    type Error = tower::BoxError;
    type Future = std::future::Pending<Result<router::Response, tower::BoxError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Pending
    }

    fn call(&mut self, _req: router::Request) -> Self::Future {
        unreachable!("a never-ready service is never called")
    }
}

#[cfg(test)]
mod tests {
    use tower::Service;

    use super::*;
    use crate::metrics::FutureMetricsExt;

    /// An unready pipeline must not stall readiness, because hyper awaits `poll_ready` inside
    /// each request's future.
    #[tokio::test]
    async fn connection_router_service_always_reports_ready() {
        let mut connection_service =
            connection_router_service(router::BoxCloneService::new(NeverReady));

        let ready = futures::future::poll_fn(|cx| {
            std::task::Poll::Ready(connection_service.poll_ready(cx))
        })
        .await;

        assert!(
            matches!(ready, std::task::Poll::Ready(Ok(()))),
            "poll_ready must never report Pending to hyper"
        );
    }

    /// The pipeline's `call` has to run on the request's own task. On a shared worker task, its
    /// synchronous work for every request would queue behind one core.
    #[tokio::test(flavor = "multi_thread")]
    async fn requests_call_the_pipeline_on_their_own_task() {
        let called_on = Arc::new(parking_lot::Mutex::new(None));
        let pipeline = router::BoxCloneService::new(tower::service_fn({
            let called_on = called_on.clone();
            move |_req: router::Request| {
                *called_on.lock() = tokio::task::try_id();
                std::future::ready(router::Response::fake_builder().build())
            }
        }));
        let service = connection_router_service(pipeline);

        let request_task = tokio::spawn(async move {
            service
                .oneshot(router::Request::fake_builder().build().unwrap())
                .await
                .expect("the request should succeed");
            tokio::task::id()
        })
        .await
        .unwrap();

        assert_eq!(*called_on.lock(), Some(request_task));
    }

    /// Once the limit of in-flight requests is reached, a new request is answered with a 503
    /// straight away, and the next one gets through once a running request finishes.
    #[tokio::test]
    async fn requests_over_the_in_flight_limit_answer_with_a_503() {
        let (mock, mut handle) = tower_test::mock::pair::<router::Request, router::Response>();
        let service = connection_router_service_with_limit(router::BoxCloneService::new(mock), 1);

        let running = tokio::spawn(
            service
                .clone()
                .oneshot(router::Request::fake_builder().build().unwrap()),
        );
        let (_request, responder) = handle.next_request().await.unwrap();

        let shed = service
            .clone()
            .oneshot(router::Request::fake_builder().build().unwrap())
            .await
            .expect("a shed request should answer, not error");
        assert_eq!(shed.response.status(), StatusCode::SERVICE_UNAVAILABLE);

        responder.send_response(router::Response::fake_builder().build().unwrap());
        running
            .await
            .unwrap()
            .expect("the running request should succeed");

        let next = tokio::spawn(service.oneshot(router::Request::fake_builder().build().unwrap()));
        let (_request, responder) = handle.next_request().await.unwrap();
        responder.send_response(router::Response::fake_builder().build().unwrap());
        let next = next
            .await
            .unwrap()
            .expect("the next request should succeed");
        assert_eq!(next.response.status(), StatusCode::OK);
    }

    /// A shed request has to come back as a response rather than an error, so callers can return
    /// it without knowing about the in-flight limit.
    #[tokio::test]
    async fn shed_requests_answer_with_a_503_graphql_error() {
        let response = shed_as_503(
            Context::new(),
            std::future::ready(Err(Overloaded::new().into())),
        )
        .await
        .expect("a shed request should answer, not error");

        assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = router::body::into_bytes(response.response.into_body())
            .await
            .expect("the body should be readable");
        let body: graphql::Response =
            serde_json::from_slice(&body).expect("the body should be a GraphQL response");

        let error = body.errors.first().expect("a GraphQL error is expected");
        assert_eq!(
            error.extensions.get("code").and_then(|code| code.as_str()),
            Some("REQUEST_CONCURRENCY_LIMITED")
        );
        assert_eq!(
            error.message,
            "Your request has been concurrency limited waiting for the router"
        );
    }

    /// The router pipeline's `http.server.*` instruments never see a shed request, so the shed
    /// path has to record the duration itself.
    #[tokio::test]
    async fn shed_requests_record_a_request_duration() {
        async {
            let _ = shed_as_503(
                Context::new(),
                std::future::ready(Err(Overloaded::new().into())),
            )
            .await;

            assert_histogram_count!(
                "http.server.request.duration",
                1,
                "http.response.status_code" = 503i64
            );
        }
        .with_metrics()
        .await;
    }

    /// Anything other than an `Overloaded` has to pass through untouched.
    #[tokio::test]
    async fn other_errors_pass_through() {
        let err = shed_as_503(
            Context::new(),
            std::future::ready(Err("something else".into())),
        )
        .await
        .expect_err("a non-overload error should stay an error");

        assert_eq!(err.to_string(), "something else");
    }
}
