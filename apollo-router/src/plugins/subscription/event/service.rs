//! Tower service boundary for event-backed subscriptions.

use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::future::Either;
use serde_json_bytes::Value;
use tokio::sync::mpsc;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tracing::Instrument;
use tracing::instrument::Instrumented;

use super::EventError;
use super::EventRuntime;
use crate::configuration::events::EventsConfiguration;
use crate::error::Error;
use crate::graphql;
use crate::plugins::subscription::SUBSCRIPTION_SUBGRAPH_NAME_CONTEXT_KEY;
use crate::plugins::subscription::SubscriptionTaskParams;
use crate::plugins::subscription::fetch::install_subscription_task;
use crate::plugins::subscription::fetch::subscription_admission_error;
use crate::query_planner::SUBSCRIBE_SPAN_NAME;
use crate::services::FetchResponse;
use crate::services::fetch::SubscriptionRequest;
use crate::services::subgraph::BoxGqlStream;
use crate::spec::Schema;

/// Routes event-backed subscriptions to an [`EventRuntime`] and delegates all other
/// subscriptions to the wrapped service.
#[derive(Clone)]
pub(crate) struct EventSubscriptionLayer {
    runtime: Arc<EventRuntime>,
}

impl EventSubscriptionLayer {
    pub(crate) fn try_new(
        schema: Arc<Schema>,
        configuration: EventsConfiguration,
    ) -> Result<Self, EventError> {
        Ok(Self {
            runtime: Arc::new(EventRuntime::try_new(schema, configuration)?),
        })
    }

    #[cfg(test)]
    pub(crate) fn new(runtime: Arc<EventRuntime>) -> Self {
        Self { runtime }
    }
}

impl<S> Layer<S> for EventSubscriptionLayer {
    type Service = EventSubscriptionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        EventSubscriptionService {
            inner,
            runtime: self.runtime.clone(),
        }
    }
}

/// Tower service produced by [`EventSubscriptionLayer`].
///
/// Readiness is delegated to the fallback service because Tower does not provide the request to
/// `poll_ready`. Event-backed requests do not otherwise call the fallback service.
#[derive(Clone)]
pub(crate) struct EventSubscriptionService<S> {
    inner: S,
    runtime: Arc<EventRuntime>,
}

impl<S> Service<SubscriptionRequest> for EventSubscriptionService<S>
where
    S: Service<SubscriptionRequest, Response = FetchResponse, Error = BoxError>,
{
    type Response = FetchResponse;
    type Error = BoxError;
    type Future =
        Either<S::Future, Instrumented<BoxFuture<'static, Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: SubscriptionRequest) -> Self::Future {
        if !self
            .runtime
            .is_event_subscription(&request.subscription_node)
        {
            return Either::Left(self.inner.call(request));
        }

        let service_name = request.subscription_node.service_name.clone();
        let fetch_time_offset = request.context.created_at.elapsed().as_nanos() as i64;
        let _ = request.context.insert(
            SUBSCRIPTION_SUBGRAPH_NAME_CONTEXT_KEY,
            service_name.to_string(),
        );

        Either::Right(
            subscribe(self.runtime.clone(), request).instrument(tracing::info_span!(
                SUBSCRIBE_SPAN_NAME,
                "otel.kind" = "INTERNAL",
                "apollo.subgraph.name" = service_name.as_ref(),
                "apollo_private.sent_time_offset" = fetch_time_offset,
                "apollo.subscription.source" = "event_stream"
            )),
        )
    }
}

fn subscribe(
    runtime: Arc<EventRuntime>,
    request: SubscriptionRequest,
) -> BoxFuture<'static, Result<FetchResponse, BoxError>> {
    Box::pin(async move {
        if let Some(error) = subscription_admission_error(request.subscription_config.as_ref()) {
            return Ok(error);
        }

        let stream = match runtime
            .subscribe(&request.subscription_node, &request.variables.variables)
            .await
        {
            Ok(stream) => stream,
            Err(error) => return Ok(event_error(error.to_string())),
        };
        let stream: BoxGqlStream = Box::pin(stream.map(|event| {
            match event {
                Ok(response) => response,
                Err(error) => graphql::Response::builder()
                    .error(
                        Error::builder()
                            .message(error.to_string())
                            .extension_code("EVENT_STREAM_ERROR")
                            .build(),
                    )
                    .build(),
            }
        }));

        install_stream(request, stream).await
    })
}

async fn install_stream(
    request: SubscriptionRequest,
    stream: BoxGqlStream,
) -> Result<FetchResponse, BoxError> {
    let Some(subscription_config) = request.subscription_config else {
        return Ok(event_error("subscription support is not enabled"));
    };
    let Some(handle) = request.subscription_handle else {
        return Ok(event_error("no subscription handle was provided"));
    };

    let (stream_sender, stream_receiver) = mpsc::channel(1);
    stream_sender
        .send(stream)
        .await
        .map_err(|error| EventError::new(error.to_string()))?;
    if let Err(response) = install_subscription_task(SubscriptionTaskParams {
        client_sender: request.sender,
        subscription_handle: handle,
        subscription_config,
        stream_rx: stream_receiver.into(),
    })
    .await
    {
        return Ok(response);
    }
    Ok((Value::default(), Vec::new()))
}

fn event_error(message: impl Into<String>) -> FetchResponse {
    (
        Value::default(),
        vec![
            Error::builder()
                .message(message.into())
                .extension_code("EVENT_SUBSCRIPTION_ERROR")
                .build(),
        ],
    )
}

#[cfg(test)]
mod tests;
