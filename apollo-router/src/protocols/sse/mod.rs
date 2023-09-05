use std::future;
use std::pin::Pin;
use std::task::Poll;

use futures::Stream;
use futures::StreamExt;
use hyper::client::connect::Connect;
use pin_project_lite::pin_project;

use super::websocket::ServerMessage;
use crate::graphql;
use crate::protocols::websocket::ServerError;

pub(crate) mod client;
pub(crate) mod config;
pub(crate) mod error;
pub(crate) mod event_parser;
pub(crate) mod retry;

pin_project! {
    pub(crate) struct GraphqlSSE<S> {
        #[pin]
        stream: S,
        id: String,
        // Booleans for state machine when closing the stream
        completed: bool,
        terminated: bool,
    }
}

impl<S> GraphqlSSE<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>>,
{
    pub(crate) fn new(stream: S, id: String) -> Result<Self, graphql::Error> {
        Ok(Self {
            stream,
            id,
            completed: false,
            terminated: false,
        })
    }
}

pub(crate) fn convert_sse_stream<C>(
    client: client::Client<C>,
    id: String,
) -> impl Stream<Item = serde_json::Result<ServerMessage>>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    client.stream().filter_map(move |msg| match msg {
        Ok(sse) => match sse {
            event_parser::Sse::Event(event) => match event.event_type.as_str() {
                "next" => future::ready(Some(serde_json::from_str(&event.data).map(|s| {
                    ServerMessage::Next {
                        id: event.id.unwrap_or_else(|| id.clone()),
                        payload: s,
                    }
                }))),
                "complete" => future::ready(Some(Ok(ServerMessage::Complete {
                    id: event.id.unwrap_or_else(|| id.clone()),
                }))),
                event_type => future::ready(Some(Ok(ServerMessage::Error {
                    id: id.to_string(),
                    payload: ServerError::Error(
                        graphql::Error::builder()
                            .message(format!("invalid event type: {event_type} received"))
                            .extension_code("SSE_INVALID_EVENT_TYPE")
                            .build(),
                    ),
                }))),
            },
            event_parser::Sse::Comment(_) => future::ready(None),
        },
        Err(err) => {
            tracing::trace!("cannot consume more message on sse stream: {err:?}");
            future::ready(Some(Ok(ServerMessage::Error {
                id: id.to_string(),
                payload: ServerError::Error(
                    graphql::Error::builder()
                        .message(format!("cannot read message from sse: {err:?}"))
                        .extension_code("SSE_MESSAGE_ERROR")
                        .build(),
                ),
            })))
        }
    })
}

impl<S> Stream for GraphqlSSE<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>>,
{
    type Item = graphql::Response;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut().project();

        match Pin::new(&mut this.stream).poll_next(cx) {
            Poll::Ready(message) => match message {
                Some(server_message) => match server_message {
                    Ok(server_message) => {
                        if let Some(id) = &server_message.id() {
                            if this.id != id {
                                tracing::error!("we should not receive data from other subscriptions, closing the stream");
                                return Poll::Ready(None);
                            }
                        }
                        match server_message.into_graphql_response() {
                            (None, true) => Poll::Ready(None),
                            // For ignored message like ACK, Ping, Pong, etc...
                            (None, false) => self.poll_next(cx),
                            (Some(resp), _) => Poll::Ready(Some(resp)),
                        }
                    }
                    Err(err) => Poll::Ready(
                        graphql::Response::builder()
                            .error(
                                graphql::Error::builder()
                                    .message(format!(
                                        "cannot deserialize sse server message: {err:?}"
                                    ))
                                    .extension_code("INVALID_SSE_SERVER_MESSAGE_FORMAT")
                                    .build(),
                            )
                            .build()
                            .into(),
                    ),
                },
                None => Poll::Ready(None),
            },
            Poll::Pending => Poll::Pending,
        }
    }
}
