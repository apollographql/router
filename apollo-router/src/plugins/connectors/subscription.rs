//! Connector-backed subscriptions: one upstream HTTP response, read as a
//! stream of events, each mapped through the connector's selection into a
//! GraphQL subscription event.

use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::StreamMode;
use apollo_federation::connectors::runtime::errors::RuntimeError;
use apollo_federation::connectors::runtime::key::ResponseKey;
use apollo_federation::connectors::runtime::responses::deserialize_response;
use apollo_federation::connectors::runtime::responses::handle_raw_event;
use apollo_federation::connectors::runtime::stream_framing::StreamEvent;
use apollo_federation::connectors::runtime::stream_framing::StreamFraming;
use futures::StreamExt;
use http::header::CONTENT_TYPE;
use http::response::Parts;
use http_body_util::BodyExt;
use mime::Mime;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::Context;
use crate::graphql;
use crate::plugins::connectors::handle_responses::declared_error_for_client;
use crate::services::router::body::RouterBody;
use crate::services::subgraph::BoxGqlStream;

/// Placed in the context's extensions by the subscription fetch, to ask the
/// connector request service to stream its response into this sender instead
/// of buffering it. The request service takes it out when it sees it, so the
/// entity fetches that later resolve each event, which share this context, are
/// not affected.
#[derive(Clone)]
pub(crate) struct ConnectorSubscriptionStream(pub(crate) mpsc::Sender<BoxGqlStream>);

/// Everything needed to map one event, cloned into the stream.
struct EventMapper {
    parts: Parts,
    key: ResponseKey,
    connector: Arc<Connector>,
    context: Context,
    supergraph_request: Arc<http::Request<graphql::Request>>,
    operation: Option<Arc<Valid<ExecutableDocument>>>,
}

impl EventMapper {
    /// Maps one event into a subscription payload, or `None` when the selection filtered it out
    /// (see [`handle_raw_event`]).
    fn map(&self, data: &Value) -> Option<graphql::Response> {
        let mut mapped = handle_raw_event(
            data,
            &self.parts,
            self.key.clone(),
            &self.connector,
            &self.context,
            self.supergraph_request.headers(),
        )?
        .apply_operation(
            self.operation
                .as_ref()
                .map(|arc_valid_doc| arc_valid_doc.as_ref().as_ref()),
            &self.connector.schema_subtypes_map,
        );

        // Errors declared with `->withError` belong to this payload. They ride in its `errors`,
        // marked, until the event's execution lifts them into its `extensions.connectorErrors`.
        let declared = mapped.take_declared_errors();
        let mut data = Map::new();
        let mut errors: Vec<RuntimeError> = Vec::new();
        if let Err(error) = mapped.add_to_data(&mut data, &mut errors, 1) {
            return Some(
                graphql::Response::builder()
                    .error(
                        graphql::Error::builder()
                            .message(error.to_string())
                            .extension_code("CONNECTOR_SUBSCRIPTION_EVENT")
                            .build(),
                    )
                    .build(),
            );
        }
        let mut errors: Vec<graphql::Error> = errors.into_iter().map(Into::into).collect();
        errors.extend(
            declared
                .into_iter()
                .filter_map(|error| declared_error_for_client(error, &self.context)),
        );
        // `subscribed` keeps the client's stream open for the next event
        Some(
            graphql::Response::builder()
                .data(Value::Object(data))
                .errors(errors)
                .subscribed(true)
                .build(),
        )
    }
}

/// Turns a connector's HTTP response into a stream of GraphQL subscription
/// events.
///
/// A streamed body (SSE or JSON Lines) yields one GraphQL event per upstream
/// event whose payload is JSON. Without `http.stream`, `$` is that one event;
/// with `stream: "array"`, it is the list of every event so far. Any other body
/// is a stream of one: it is read in full and mapped once, so a plain JSON
/// endpoint behaves as a subscription that emits a single event and completes.
/// An empty body emits nothing.
pub(crate) fn stream_response(
    response: http::Response<RouterBody>,
    key: ResponseKey,
    connector: Arc<Connector>,
    context: Context,
    supergraph_request: Arc<http::Request<graphql::Request>>,
    operation: Option<Arc<Valid<ExecutableDocument>>>,
) -> BoxGqlStream {
    let (parts, body) = response.into_parts();
    let framing = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok()?.parse::<Mime>().ok())
        .and_then(|mime| StreamFraming::from_content_type(&mime));
    let mapper = Arc::new(EventMapper {
        parts,
        key,
        connector,
        context,
        supergraph_request,
        operation,
    });

    // The body is read by its own task, because `BoxGqlStream` must be `Sync` and a body is not.
    // The bounded channel also gives backpressure: a slow client stops the task reading. A client
    // that unsubscribes drops the receiver, which the task watches for while it waits on the
    // body, so it returns and hangs up on the upstream at once, even if the upstream is idle or
    // sending only heartbeats and no event is ever delivered.
    let stream_mode = mapper
        .connector
        .transport
        .as_ref()
        .and_then(|transport| transport.stream);
    let (sender, receiver) = mpsc::channel(EVENT_BUFFER);
    tokio::spawn(async move {
        let Some(framing) = framing else {
            let event = tokio::select! {
                biased;
                _ = sender.closed() => return,
                collected = body.collect() => match collected {
                    Ok(body) => {
                        let body = body.to_bytes();
                        if body.is_empty() {
                            // Nothing arrived, so there is no event to emit
                            return;
                        }
                        // The same folding as a query: under `array`, a plain body is a list of one
                        match deserialize_response(&body, &mapper.parts.headers, stream_mode) {
                            Ok(data) => mapper.map(&data),
                            Err(_) => Some(invalid_body_error()),
                        }
                    }
                    Err(_) => Some(body_error()),
                },
            };
            if let Some(event) = event {
                let _ = sender.send(event).await;
            }
            return;
        };

        // Under `array`, every event so far, which each GraphQL event maps as `$`
        let mut events_so_far = Vec::new();
        let mut body = body.into_data_stream();
        let mut decoder = framing.decoder();
        loop {
            let next = tokio::select! {
                biased;
                _ = sender.closed() => return,
                next = body.next() => next,
            };
            let (events, done) = match next {
                Some(Ok(chunk)) => (decoder.push(&chunk), false),
                Some(Err(_)) => {
                    let _ = sender.send(body_error()).await;
                    return;
                }
                None => (decoder.finish(), true),
            };
            // An event whose payload isn't JSON, like a `[DONE]` sentinel, is not data
            for data in events.iter().filter_map(StreamEvent::json) {
                let mapped = match stream_mode {
                    None => mapper.map(&data),
                    Some(StreamMode::Array) => {
                        events_so_far.push(data);
                        mapper.map(&Value::Array(events_so_far.clone()))
                    }
                };
                // An event the selection filtered out sends nothing
                if let Some(mapped) = mapped
                    && sender.send(mapped).await.is_err()
                {
                    return;
                }
            }
            if done {
                return;
            }
        }
    });

    Box::pin(ReceiverStream::new(receiver))
}

/// How many mapped events may wait for a slow client before the upstream stops being read.
const EVENT_BUFFER: usize = 16;

fn body_error() -> graphql::Response {
    graphql::Response::builder()
        .error(
            graphql::Error::builder()
                .message("The connector's response stream ended with an error.")
                .extension_code("CONNECTOR_SUBSCRIPTION_STREAM")
                .build(),
        )
        .subscribed(false)
        .build()
}

fn invalid_body_error() -> graphql::Response {
    graphql::Response::builder()
        .error(
            graphql::Error::builder()
                .message("The connector's response body was not valid JSON.")
                .extension_code("CONNECTOR_SUBSCRIPTION_INVALID_BODY")
                .build(),
        )
        .subscribed(false)
        .build()
}
