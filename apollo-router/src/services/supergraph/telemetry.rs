use futures::StreamExt as _;
use futures::TryFutureExt as _;
use futures::future::BoxFuture;
use futures::future::Either;
use opentelemetry::Key;
use opentelemetry::KeyValue;

use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::supergraph::events::SupergraphEventResponse;
use crate::plugins::telemetry::config_new::supergraph::selectors::FIRST_EVENT_CONTEXT_KEY;
use crate::services::supergraph;

/// Populate the `apollo::supergraph::first_event` response context key for user extensibility.
#[derive(Clone)]
pub(super) struct PopulateFirstEventContextLayer {
    _private: (),
}

impl PopulateFirstEventContextLayer {
    pub(super) fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for PopulateFirstEventContextLayer {
    type Service = PopulateFirstEventContextService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PopulateFirstEventContextService::new(inner)
    }
}

#[derive(Clone)]
pub(super) struct PopulateFirstEventContextService<S> {
    inner: S,
}

impl<S> PopulateFirstEventContextService<S> {
    pub(super) fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<R, S> tower::Service<R> for PopulateFirstEventContextService<S>
where
    S: tower::Service<R, Response = supergraph::Response>,
{
    type Response = supergraph::Response;
    type Error = S::Error;
    type Future =
        futures::future::MapOk<S::Future, fn(supergraph::Response) -> supergraph::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        let fut = self.inner.call(req);

        fut.map_ok(populate_first_event_context)
    }
}

fn populate_first_event_context(response: supergraph::Response) -> supergraph::Response {
    let supergraph::Response { response, context } = response;
    let (parts, response_stream) = response.into_parts();

    let mut first_event = true;
    let mut inserted = false;
    let ctx = context.clone();
    let response_stream = response_stream
        .inspect(move |_| {
            if first_event {
                // Populate FIRST_EVENT_CONTEXT_KEY so downstream telemetry selectors
                // (SupergraphSelector::IsPrimaryResponse) can distinguish the primary
                // response chunk from deferred/subscription chunks.
                ctx.insert_json_value(FIRST_EVENT_CONTEXT_KEY, serde_json_bytes::Value::Bool(true));
                first_event = false;
            } else if !inserted {
                ctx.insert_json_value(
                    FIRST_EVENT_CONTEXT_KEY,
                    serde_json_bytes::Value::Bool(false),
                );
                inserted = true;
            }
        })
        .boxed();

    supergraph::Response {
        response: http::Response::from_parts(parts, response_stream),
        context,
    }
}

#[derive(Clone)]
pub(super) struct LogResponseLayer {
    _private: (),
}

impl LogResponseLayer {
    pub(super) fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for LogResponseLayer {
    type Service = LogResponseService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LogResponseService::new(inner)
    }
}

// TODO(@goto-bus-stop): this should live in the same place as request logging probably?
#[derive(Clone)]
pub(super) struct LogResponseService<S> {
    inner: S,
}

impl<S> LogResponseService<S> {
    pub(super) fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> tower::Service<supergraph::Request> for LogResponseService<S>
where
    S: tower::Service<supergraph::Request, Response = supergraph::Response>,
    S::Future: Send + 'static,
{
    type Response = supergraph::Response;
    type Error = S::Error;
    type Future = Either<S::Future, BoxFuture<'static, Result<Self::Response, Self::Error>>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: supergraph::Request) -> Self::Future {
        let supergraph_response_event = req
            .context
            .extensions()
            .with_lock(|lock| lock.get::<SupergraphEventResponse>().cloned());

        let fut = self.inner.call(req);

        let Some(supergraph_response_event) = supergraph_response_event else {
            return Either::Left(fut);
        };

        Either::Right(Box::pin(async move {
            let response = fut.await?;
            let supergraph::Response { response, context } = response;
            let (parts, response_stream) = response.into_parts();

            // make sure to resolve the first part of the stream - that way we know context
            // variables (`FIRST_EVENT_CONTEXT_KEY`, `CONTAINS_GRAPHQL_ERROR`) have been set
            let (first, remaining) = response_stream.into_future().await;
            let response_stream =
                futures::stream::once(std::future::ready(first.unwrap_or_default()))
                    .chain(remaining)
                    .boxed();

            let mut attrs = Vec::with_capacity(4);
            let header_string = crate::services::header_masking::masked_headers_for_log(
                &context,
                crate::services::header_masking::Direction::Response,
                None,
                &parts.headers,
            );
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.headers"),
                opentelemetry::Value::String(header_string.into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.status"),
                opentelemetry::Value::String(format!("{}", parts.status).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.version"),
                opentelemetry::Value::String(format!("{:?}", parts.version).into()),
            ));
            let ctx = context.clone();
            let response_stream = response_stream
                .inspect(move |resp| {
                    if !supergraph_response_event
                        .condition
                        .evaluate_event_response(resp, &ctx)
                    {
                        return;
                    }
                    attrs.push(KeyValue::new(
                        Key::from_static_str("http.response.body"),
                        opentelemetry::Value::String(
                            serde_json::to_string(resp).unwrap_or_default().into(),
                        ),
                    ));
                    log_event(
                        supergraph_response_event.level,
                        "supergraph.response",
                        attrs.clone(),
                        "",
                    );
                })
                .boxed();

            Ok(supergraph::Response {
                context,
                response: http::Response::from_parts(parts, response_stream.boxed()),
            })
        }))
    }
}
