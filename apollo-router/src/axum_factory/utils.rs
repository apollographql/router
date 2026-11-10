//! Utilities used for [`super::AxumHttpServerFactory`]

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use futures::future::Either;
use futures::future::Ready;
use futures::future::ready;
use http::StatusCode;
use opentelemetry::global;
use opentelemetry::trace::TraceContextExt;
use pin_project_lite::pin_project;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tower::BoxError;
use tower::Service;
use tower_http::trace::MakeSpan;
use tracing::Span;

use crate::Context;
use crate::graphql;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::pool::Pooled;
use crate::layers::pool::PooledFuture;
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

/// Every connection's share of one router pipeline. A request takes a copy of the pipeline from
/// a pool (`layers::pool`), readies and calls it on its own task and returns it, so the
/// pipeline's synchronous work runs on every request's own task at once; a request past
/// [`DEFAULT_BUFFER_SIZE`] in progress on the listen address is answered a 503 at once. Its
/// clone, made per connection and per request, is two reference counts'.
///
/// Where a buffer stood here, its one worker task readied and called the pipeline for every
/// request on every connection, a channel's hop and a wake per request, and its queue bounded the
/// requests waiting for that worker; the bound now holds the requests in progress, readied or
/// awaiting their response, against the same number.
#[derive(Clone)]
pub(crate) struct ConnectionRouterService {
    pipeline: Pooled<router::BoxCloneService>,
    in_progress: Arc<Semaphore>,
}

/// Wraps a router service so every connection can share one instance.
pub(crate) fn connection_router_service(
    service: router::BoxCloneService,
) -> ConnectionRouterService {
    ConnectionRouterService {
        pipeline: Pooled::new(service),
        in_progress: Arc::new(Semaphore::new(DEFAULT_BUFFER_SIZE)),
    }
}

impl Service<router::Request> for ConnectionRouterService {
    type Response = router::Response;
    type Error = BoxError;
    type Future = Either<
        Ready<Result<router::Response, BoxError>>,
        InProgress<PooledFuture<router::BoxCloneService, router::Request>>,
    >;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), BoxError>> {
        // hyper awaits `poll_ready` inside each request's future, so `Pending` here would stall the
        // request rather than push back on its connection: `call` decides whether to shed
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: router::Request) -> Self::Future {
        // `try_acquire` never waits, so tokio's cooperative budget cannot shed a request the
        // bound had room for
        match Arc::clone(&self.in_progress).try_acquire_owned() {
            Ok(permit) => Either::Right(InProgress {
                inner: self.pipeline.call(request),
                _permit: permit,
            }),
            Err(_) => Either::Left(ready(Ok(shed(request.context)))),
        }
    }
}

pin_project! {
    /// A request's future, holding its place under the bound until it completes.
    pub(crate) struct InProgress<F> {
        #[pin]
        inner: F,
        _permit: OwnedSemaphorePermit,
    }
}

impl<F: Future> Future for InProgress<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx)
    }
}

/// The answer to a request shed past the bound, a 503 its caller returns as any other.
fn shed(context: Context) -> router::Response {
    // Shedding happens before the router pipeline, and its `http.server.*` instruments with it,
    // so record the duration here to keep shed requests visible.
    record_bypassed_request(
        StatusCode::SERVICE_UNAVAILABLE.as_u16(),
        context.created_at.elapsed(),
    );

    // Debug rather than warn: shedding is per-request, so a sustained overload would otherwise
    // emit a line per rejected request while the router is already struggling.
    tracing::debug!(
        code = "REQUEST_CONCURRENCY_LIMITED",
        "the router is at its bound of requests in progress, shedding request",
    );

    router::Response::error_builder()
        .status_code(StatusCode::SERVICE_UNAVAILABLE)
        .error(
            graphql::Error::builder()
                .message("Your request has been concurrency limited waiting for the router")
                .extension_code("REQUEST_CONCURRENCY_LIMITED")
                .build(),
        )
        .context(context)
        .build()
        .expect("overloaded response should build")
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

    /// The tasks a pipeline's `poll_ready` and `call` ran on.
    #[derive(Clone, Default)]
    struct TaskRecorder {
        readied_on: Arc<parking_lot::Mutex<Vec<Option<tokio::task::Id>>>>,
        called_on: Arc<parking_lot::Mutex<Vec<Option<tokio::task::Id>>>>,
    }

    impl tower::Service<router::Request> for TaskRecorder {
        type Response = router::Response;
        type Error = tower::BoxError;
        type Future = std::future::Ready<Result<router::Response, tower::BoxError>>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.readied_on.lock().push(tokio::task::try_id());
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: router::Request) -> Self::Future {
            self.called_on.lock().push(tokio::task::try_id());
            std::future::ready(
                router::Response::fake_builder()
                    .context(request.context)
                    .build(),
            )
        }
    }

    /// A request readies and calls the pipeline on its own task: where a buffer stood, its one
    /// worker task readied and called the pipeline for every request on every connection.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_readies_and_calls_the_pipeline_on_its_own_task() {
        use tower::ServiceExt as _;

        let recorder = TaskRecorder::default();
        let service = connection_router_service(router::BoxCloneService::new(recorder.clone()));

        let request_task = tokio::spawn(async move {
            service
                .oneshot(router::Request::fake_builder().build().expect("a request"))
                .await
                .expect("an answer");
            tokio::task::id()
        })
        .await
        .expect("the request's task");

        assert_eq!(
            (
                recorder.readied_on.lock().clone(),
                recorder.called_on.lock().clone()
            ),
            (vec![Some(request_task)], vec![Some(request_task)]),
            "the tasks the pipeline was readied and called on, against the request's own"
        );
    }

    /// A request past the router's bound answers a 503 at once, while the requests within the
    /// bound wait on a pipeline that is never ready: every connection shares the one bound.
    #[tokio::test]
    async fn a_request_past_the_bound_answers_a_503_at_once() {
        use tower::ServiceExt as _;

        use crate::layers::DEFAULT_BUFFER_SIZE;

        let service = connection_router_service(router::BoxCloneService::new(NeverReady));
        let context = Context::new();
        let request = || {
            service.clone().oneshot(
                router::Request::fake_builder()
                    .context(context.clone())
                    .build()
                    .expect("a request"),
            )
        };
        let mut within = Vec::with_capacity(DEFAULT_BUFFER_SIZE);
        for _ in 0..DEFAULT_BUFFER_SIZE {
            let mut future = Box::pin(request());
            assert!(
                futures::poll!(future.as_mut()).is_pending(),
                "a request within the bound waits on the pipeline"
            );
            within.push(future);
        }
        // the request at the bound: a buffer's worker held one more request beside its queue
        let mut at_the_bound = Box::pin(request());
        let _ = futures::poll!(at_the_bound.as_mut());

        let past = request().await.expect("a shed request answers");

        assert_eq!(past.response.status(), StatusCode::SERVICE_UNAVAILABLE);
        for mut future in within {
            assert!(
                futures::poll!(future.as_mut()).is_pending(),
                "a request within the bound still waits"
            );
        }
    }

    /// A shed request comes back as a response rather than an error, so callers can return it
    /// without knowing about the bound.
    #[tokio::test]
    async fn shed_requests_answer_with_a_503_graphql_error() {
        let response = shed(Context::new());

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
            let _ = shed(Context::new());

            assert_histogram_count!(
                "http.server.request.duration",
                1,
                "http.response.status_code" = 503i64
            );
        }
        .with_metrics()
        .await;
    }
}
