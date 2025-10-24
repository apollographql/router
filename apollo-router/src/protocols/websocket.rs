//! Implements WebSocket _client_ protocols for GraphQL subscriptions.

use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use futures::Future;
use futures::Sink;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use futures::future;
use futures::stream::SplitStream;
use http::HeaderValue;
use pin_project_lite::pin_project;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio_stream::wrappers::IntervalStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::graphql;

const CONNECTION_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// The WebSocket subprotocol name for the modern graphql-ws protocol.
/// See [`WebSocketProtocol::GraphqlWs`].
const GRAPHQL_WS_SUBPROTOCOL: &str = "graphql-transport-ws";
/// The WebSocket subprotocol name for the legacy subscriptions-transport-ws protocol.
/// See [`WebSocketProtocol::SubscriptionsTransportWs`].
const SUBSCRIPTIONS_TRANSPORT_WS_SUBPROTOCOL: &str = "graphql-ws";

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebSocketProtocol {
    /// The modern graphql-ws protocol. The subprotocol name is "graphql-transport-ws".
    ///
    /// Spec URL: https://github.com/enisdenjo/graphql-ws/blob/0c0eb499c3a0278c6d9cc799064f22c5d24d2f60/PROTOCOL.md
    #[default]
    GraphqlWs,
    #[serde(rename = "graphql_transport_ws")]
    /// The legacy subscriptions-transport-ws protocol. Confusingly, the subprotocol name is
    /// "graphql-ws".
    ///
    /// https://github.com/apollographql/subscriptions-transport-ws/blob/36f3f6f780acc1a458b768db13fd39c65e5e6518/PROTOCOL.md
    SubscriptionsTransportWs,
}

impl From<WebSocketProtocol> for HeaderValue {
    fn from(value: WebSocketProtocol) -> Self {
        match value {
            WebSocketProtocol::GraphqlWs => HeaderValue::from_static(GRAPHQL_WS_SUBPROTOCOL),
            WebSocketProtocol::SubscriptionsTransportWs => {
                HeaderValue::from_static(SUBSCRIPTIONS_TRANSPORT_WS_SUBPROTOCOL)
            }
        }
    }
}

impl WebSocketProtocol {
    /// Returns a subscription start message appropriate for the active protocol.
    fn subscribe(&self, id: String, payload: graphql::Request) -> ClientMessage {
        match self {
            WebSocketProtocol::GraphqlWs => ClientMessage::Subscribe { id, payload },
            WebSocketProtocol::SubscriptionsTransportWs => ClientMessage::OldStart { id, payload },
        }
    }

    /// Returns a subscription completion message appropriate for the active protocol.
    fn complete(&self, id: String) -> ClientMessage {
        match self {
            WebSocketProtocol::GraphqlWs => ClientMessage::Complete { id },
            WebSocketProtocol::SubscriptionsTransportWs => ClientMessage::OldStop { id },
        }
    }
}

/// WebSocket messages sent from the client.
///
/// Branches prefixed with "Old" are specific to the subscriptions-transport-ws protocol, other
/// branches are either part of the graphql-ws protocol or shared by both protocols.
#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    /// A new connection
    ConnectionInit {
        /// Optional init payload from the client
        payload: Option<serde_json_bytes::Value>,
    },
    /// The start of a Websocket subscription in the graphql-ws protocol
    Subscribe {
        /// Message ID
        id: String,
        /// The GraphQL Request - this can be modified by protocol implementors
        /// to add files uploads.
        payload: graphql::Request,
    },
    /// The start of a Websocket subscription in the subscriptions-transport-ws protocol
    #[serde(rename = "start")]
    OldStart {
        /// Message ID
        id: String,
        /// The GraphQL Request - this can be modified by protocol implementors
        /// to add files uploads.
        payload: graphql::Request,
    },
    /// The end of a Websocket subscription in the graphql-ws protocol
    Complete {
        /// Message ID
        id: String,
    },
    /// The end of a Websocket subscription in the subscriptions-transport-ws protocol
    #[serde(rename = "stop")]
    OldStop {
        /// Message ID
        id: String,
    },
    /// Connection terminated by the client, only used in the subscriptions-transport-ws protocol.
    #[serde(rename = "connection_terminate")]
    OldConnectionTerminate,
    /// Close the websocket connection. This is a router-internal message, not part of the protocol
    CloseWebsocket,
    /// Useful for detecting failed connections, displaying latency metrics or
    /// other types of network probing.
    ///
    /// Reference: <https://github.com/enisdenjo/graphql-ws/blob/0c0eb499c3a0278c6d9cc799064f22c5d24d2f60/PROTOCOL.md#ping>
    Ping {
        /// Additional details about the ping.
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json_bytes::Value>,
    },
    /// The response to the Ping message.
    ///
    /// Reference: <https://github.com/enisdenjo/graphql-ws/blob/0c0eb499c3a0278c6d9cc799064f22c5d24d2f60/PROTOCOL.md#pong>
    Pong {
        /// Additional details about the pong.
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json_bytes::Value>,
    },
}

/// WebSocket messages received from the server.
#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerMessage {
    ConnectionAck,
    /// The payload message has type "next" in the graphql-ws protocol, and type "data" in the
    /// subscriptions-transport-ws protocol.
    #[serde(alias = "data")]
    Next {
        id: String,
        payload: graphql::Response,
    },
    #[serde(alias = "connection_error")]
    Error {
        id: Option<String>,
        payload: ServerError,
    },
    Complete {
        id: String,
    },
    #[serde(alias = "ka")]
    KeepAlive,
    /// The response to the Ping message.
    ///
    /// Reference: <https://github.com/enisdenjo/graphql-ws/blob/0c0eb499c3a0278c6d9cc799064f22c5d24d2f60/PROTOCOL.md#pong>
    Pong {
        payload: Option<serde_json::Value>,
    },
    Ping {
        payload: Option<serde_json::Value>,
    },
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub(crate) enum ServerError {
    Error(graphql::Error),
    Errors(Vec<graphql::Error>),
}

impl From<ServerError> for Vec<graphql::Error> {
    fn from(value: ServerError) -> Self {
        match value {
            ServerError::Error(e) => vec![e],
            ServerError::Errors(e) => e,
        }
    }
}

impl ServerMessage {
    fn into_graphql_response(self) -> (Option<graphql::Response>, bool) {
        match self {
            ServerMessage::Next { id: _, mut payload } => {
                payload.subscribed = Some(true);
                (Some(payload), false)
            }
            ServerMessage::Error { id: _, payload } => (
                Some(
                    graphql::Response::builder()
                        .errors(payload.into())
                        .subscribed(false)
                        .build(),
                ),
                true,
            ),
            ServerMessage::Complete { .. } => (None, true),
            ServerMessage::ConnectionAck | ServerMessage::Pong { .. } => (None, false),
            ServerMessage::Ping { .. } => (None, false),
            ServerMessage::KeepAlive => (None, false),
        }
    }

    fn id(&self) -> Option<String> {
        match self {
            ServerMessage::ConnectionAck
            | ServerMessage::KeepAlive
            | ServerMessage::Ping { .. }
            | ServerMessage::Pong { .. } => None,
            ServerMessage::Next { id, .. } | ServerMessage::Complete { id } => Some(id.to_string()),
            ServerMessage::Error { id, .. } => id.clone(),
        }
    }
}

pub(crate) struct GraphqlWebSocket<S> {
    stream: S,
    id: String,
    protocol: WebSocketProtocol,
}

impl<S> GraphqlWebSocket<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>>
        + Sink<ClientMessage>
        + std::marker::Unpin
        + std::marker::Send
        + 'static,
{
    pub(crate) async fn new(
        mut stream: S,
        id: String,
        protocol: WebSocketProtocol,
        connection_params: Option<Value>,
    ) -> Result<Self, graphql::Error> {
        let connection_init_msg = match connection_params {
            Some(connection_params) => ClientMessage::ConnectionInit {
                payload: Some(serde_json_bytes::json!({
                    "connectionParams": connection_params
                })),
            },
            None => ClientMessage::ConnectionInit { payload: None },
        };
        stream.send(connection_init_msg).await.map_err(|_err| {
            graphql::Error::builder()
                .message("cannot send connection init through websocket connection")
                .extension_code("WEBSOCKET_INIT_ERROR")
                .build()
        })?;

        let first_non_ping_payload = async {
            loop {
                match stream.next().await {
                    Some(Ok(ServerMessage::Ping { .. })) => {
                        // There's no need to send a pong here because the server will send a pong automatically.
                        // See https://docs.rs/tungstenite/latest/tungstenite/protocol/struct.WebSocket.html#method.write
                    }
                    other => {
                        return other;
                    }
                }
            }
        };

        let resp = tokio::time::timeout(CONNECTION_ACK_TIMEOUT, first_non_ping_payload)
            .await
            .map_err(|_| {
                graphql::Error::builder()
                    .message("cannot receive connection ack from websocket connection")
                    .extension_code("WEBSOCKET_ACK_ERROR_TIMEOUT")
                    .build()
            })?;
        if !matches!(resp, Some(Ok(ServerMessage::ConnectionAck))) {
            return Err(graphql::Error::builder()
                .message(format!("didn't receive the connection ack from websocket connection but instead got: {resp:?}"))
                .extension_code("WEBSOCKET_ACK_ERROR")
                .build());
        }

        Ok(Self {
            stream,
            id,
            protocol,
        })
    }

    pub(crate) async fn into_subscription(
        mut self,
        request: graphql::Request,
        heartbeat_interval: Option<tokio::time::Duration>,
    ) -> Result<SubscriptionStream<S>, graphql::Error> {
        self.stream
            .send(self.protocol.subscribe(self.id.to_string(), request))
            .await
            .map(|_| {
                SubscriptionStream::new(self.stream, self.id, self.protocol, heartbeat_interval)
            })
            .map_err(|_err| {
                graphql::Error::builder()
                    .message("cannot send to websocket connection")
                    .extension_code("WEBSOCKET_CONNECTION_ERROR")
                    .build()
            })
    }
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("websocket error")]
    WebSocketError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("deserialization/serialization error")]
    SerdeError(#[from] serde_json::Error),
}

/// Convert a bidirectional stream of untyped websocket packets to a [Stream] + [Sink] that speaks the
/// GraphQL WebSocket protocol ([`ServerMessage`] and [`ClientMessage`]).
pub(crate) fn convert_websocket_stream<T>(
    stream: WebSocketStream<T>,
    id: String,
) -> impl Stream<Item = serde_json::Result<ServerMessage>> + Sink<ClientMessage, Error = Error>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    stream
        // Serialize messages being written into the `Sink`
        .with(|client_message: ClientMessage| {
            match client_message {
                ClientMessage::CloseWebsocket => {
                    future::ready(Ok(Message::Close(Some(CloseFrame{
                        code: CloseCode::Normal,
                        reason: Default::default(),
                    }))))
                },
                message => {
                    future::ready(match serde_json::to_string(&message) {
                        Ok(client_message_str) => Ok(Message::text(client_message_str)),
                        Err(err) => Err(Error::SerdeError(err)),
                    })
                },
            }
        })
        .inspect(|msg| if let Ok(Message::Text(_) | Message::Binary(_)) = msg {
            u64_counter!(
                "apollo.router.operations.subscriptions.events",
                "Number of subscription events",
                1,
                subscriptions.mode = "passthrough"
            );
        })
        // Parse messages received from the `Stream`
        .map(move |msg| match msg {
            Ok(Message::Text(text)) => serde_json::from_str(&text),
            Ok(Message::Binary(bin)) => serde_json::from_slice(&bin),
            Ok(Message::Ping(payload)) => Ok(ServerMessage::Ping {
                payload: serde_json::from_slice(&payload).ok(),
            }),
            Ok(Message::Pong(payload)) => Ok(ServerMessage::Pong {
                payload: serde_json::from_slice(&payload).ok(),
            }),
            Ok(Message::Close(None)) => Ok(ServerMessage::Complete { id: id.to_string() }),
            Ok(Message::Close(Some(CloseFrame{ code, reason }))) => {
                if code == CloseCode::Normal {
                    Ok(ServerMessage::Complete { id: id.to_string() })
                } else {
                    Ok(ServerMessage::Error {
                        id: Some(id.to_string()),
                        payload: ServerError::Error(
                            graphql::Error::builder()
                                .message(format!("websocket connection has been closed with error code '{code}' and reason '{reason}'"))
                                .extension_code("WEBSOCKET_CLOSE_ERROR")
                                .build(),
                        ),
                    })
                }
            }
            Ok(Message::Frame(frame)) => serde_json::from_slice(frame.payload()),
            Err(err) => {
                tracing::trace!("cannot consume more message on websocket stream: {err:?}");

                Ok(ServerMessage::Error {
                    id: Some(id.to_string()),
                    payload: ServerError::Error(
                        graphql::Error::builder()
                            .message("cannot read message from websocket")
                            .extension_code("WEBSOCKET_MESSAGE_ERROR")
                            .build(),
                    ),
                })
            }
        })
}

pub(crate) struct SubscriptionStream<S> {
    inner_stream: SplitStream<InnerStream<S>>,
    close_signal: Option<tokio::sync::oneshot::Sender<()>>,
}

impl<S> SubscriptionStream<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>>
        + Sink<ClientMessage>
        + std::marker::Unpin
        + std::marker::Send
        + 'static,
{
    pub(crate) fn new(
        stream: S,
        id: String,
        protocol: WebSocketProtocol,
        heartbeat_interval: Option<tokio::time::Duration>,
    ) -> Self {
        let (mut sink, inner_stream) = InnerStream::new(stream, id, protocol).split();
        let (close_signal, close_sentinel) = tokio::sync::oneshot::channel::<()>();

        tokio::task::spawn(async move {
            if let (WebSocketProtocol::GraphqlWs, Some(duration)) = (protocol, heartbeat_interval) {
                let mut interval =
                    tokio::time::interval_at(tokio::time::Instant::now() + duration, duration);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut heartbeat_stream = IntervalStream::new(interval)
                    .map(|_| Ok(ClientMessage::Ping { payload: None }))
                    .take_until(close_sentinel);
                if let Err(err) = sink.send_all(&mut heartbeat_stream).await {
                    tracing::trace!("cannot send heartbeat: {err:?}");
                    if let Some(close_sentinel) = heartbeat_stream.take_future()
                        && let Err(err) = close_sentinel.await
                    {
                        tracing::trace!("cannot shutdown sink: {err:?}");
                    }
                }
            } else if let Err(err) = close_sentinel.await {
                tracing::trace!("cannot shutdown sink: {err:?}");
            };

            u64_counter!(
                "apollo.router.operations.subscriptions.events",
                "Number of subscription events",
                1,
                subscriptions.mode = "passthrough",
                subscriptions.complete = true
            );

            if let Err(err) = sink.close().await {
                tracing::trace!("cannot close the websocket stream: {err:?}");
            }
        });

        Self {
            inner_stream,
            close_signal: Some(close_signal),
        }
    }
}

impl<S> Drop for SubscriptionStream<S> {
    fn drop(&mut self) {
        if let Some(close_signal) = self.close_signal.take()
            && let Err(err) = close_signal.send(())
        {
            tracing::trace!("cannot close the websocket stream: {err:?}");
        }
    }
}

impl<S> Stream for SubscriptionStream<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>> + Sink<ClientMessage> + std::marker::Unpin,
{
    type Item = graphql::Response;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner_stream.poll_next_unpin(cx)
    }
}

pin_project! {
    /// A wrapper over a stream + sink speaking a GraphQL websocket protocol that:
    /// - turns internal errors into GraphQL errors
    /// - filters out messages not related to this stream's subscription ID
    /// - handles connection shutdown according to the GraphQL websocket protocols
    struct InnerStream<S> {
        #[pin]
        stream: S,
        id: String,
        protocol: WebSocketProtocol,
        // Booleans for state machine when closing the stream
        completed: bool,
        terminated: bool,
        // When the websocket stream is closed (!= graphql sub protocol)
        closed: bool,
    }
}

impl<S> InnerStream<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>> + Sink<ClientMessage> + std::marker::Unpin,
{
    fn new(stream: S, id: String, protocol: WebSocketProtocol) -> Self {
        Self {
            stream,
            id,
            protocol,
            completed: false,
            terminated: false,
            closed: false,
        }
    }
}

impl<S> Stream for InnerStream<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>> + Sink<ClientMessage>,
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
                        if let Some(id) = &server_message.id()
                            && this.id != id
                        {
                            tracing::error!(
                                "we should not receive data from other subscriptions, closing the stream"
                            );
                            return Poll::Ready(None);
                        }
                        if let ServerMessage::Ping { .. } = server_message {
                            // Send pong asynchronously
                            // XXX(@goto-bus-stop): We have to pull_flush() to ensure this thing
                            // finishes, not sure if we're doing that right now?
                            let _ = Pin::new(
                                &mut Pin::new(&mut this.stream)
                                    .send(ClientMessage::Pong { payload: None }),
                            )
                            .poll(cx);
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
                                        "cannot deserialize websocket server message: {err:?}"
                                    ))
                                    .extension_code("INVALID_WEBSOCKET_SERVER_MESSAGE_FORMAT")
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

impl<S> Sink<ClientMessage> for InnerStream<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>> + Sink<ClientMessage>,
{
    type Error = graphql::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();

        match Pin::new(&mut this.stream).poll_ready(cx) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(_err)) => Poll::Ready(Err("websocket connection error")),
            Poll::Pending => Poll::Pending,
        }
        .map_err(|err| {
            graphql::Error::builder()
                .message(format!("cannot establish websocket connection: {err}"))
                .extension_code("WEBSOCKET_CONNECTION_ERROR")
                .build()
        })
    }

    fn start_send(self: Pin<&mut Self>, item: ClientMessage) -> Result<(), Self::Error> {
        let mut this = self.project();

        Pin::new(&mut this.stream).start_send(item).map_err(|_err| {
            graphql::Error::builder()
                .message("cannot send to websocket connection")
                .extension_code("WEBSOCKET_CONNECTION_ERROR")
                .build()
        })
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        Pin::new(&mut this.stream).poll_flush(cx).map_err(|_err| {
            graphql::Error::builder()
                .message("cannot flush to websocket connection")
                .extension_code("WEBSOCKET_CONNECTION_ERROR")
                .build()
        })
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        if !*this.completed {
            // XXX(@goto-bus-stop): We have to pull_flush() to ensure this thing
            // finishes, not sure if we're doing that right now?
            match Pin::new(
                &mut Pin::new(&mut this.stream).send(this.protocol.complete(this.id.to_string())),
            )
            .poll(cx)
            {
                Poll::Ready(_) => {
                    *this.completed = true;
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
        if let WebSocketProtocol::SubscriptionsTransportWs = this.protocol
            && !*this.terminated
        {
            // XXX(@goto-bus-stop): We have to pull_flush() to ensure this thing
            // finishes, not sure if we're doing that right now?
            match Pin::new(
                &mut Pin::new(&mut this.stream).send(ClientMessage::OldConnectionTerminate),
            )
            .poll(cx)
            {
                Poll::Ready(_) => {
                    *this.terminated = true;
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }

        if !*this.closed {
            // instead of just calling poll_close we also send a proper CloseWebsocket event to indicate it's a normal close, not an error
            // XXX(@goto-bus-stop): We have to pull_flush() to ensure this thing
            // finishes, not sure if we're doing that right now?
            match Pin::new(&mut Pin::new(&mut this.stream).send(ClientMessage::CloseWebsocket))
                .poll(cx)
            {
                Poll::Ready(_) => {
                    *this.closed = true;
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }

        Pin::new(&mut this.stream).poll_close(cx).map_err(|_err| {
            graphql::Error::builder()
                .message("cannot close websocket connection")
                .extension_code("WEBSOCKET_CONNECTION_ERROR")
                .build()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;

    use axum::Router;
    use axum::extract::WebSocketUpgrade;
    use axum::extract::ws::Message as AxumWsMessage;
    use axum::routing::get;
    use bytes::Bytes;
    use futures::FutureExt;
    use http::HeaderValue;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use uuid::Uuid;

    use super::*;
    use crate::assert_response_eq_ignoring_error_id;
    use crate::graphql::Request;
    use crate::metrics::FutureMetricsExt;

    async fn emulate_correct_websocket_server_new_protocol(
        send_ping: bool,
        heartbeat_interval: Option<tokio::time::Duration>,
        port: Option<u16>,
    ) -> SocketAddr {
        let ws_handler = move |ws: WebSocketUpgrade| async move {
            let res = ws.protocols([GRAPHQL_WS_SUBPROTOCOL]).on_upgrade(move |mut socket| async move {
                let connection_ack = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                if let ClientMessage::ConnectionInit { payload } = ack_msg {
                    assert_eq!(payload, Some(serde_json_bytes::json!({"connectionParams": {
                        "token": "XXX"
                    }})));
                } else {
                   panic!("it should be a connection init message");
                }

                if send_ping {
                    // It turns out some servers may send Pings before they even ack the connection.
                    socket
                        .send(AxumWsMessage::Ping(Bytes::new()))
                        .await
                        .unwrap();

                    let pong_message = socket.next().await.unwrap().unwrap();
                    assert_eq!(pong_message, AxumWsMessage::Pong(Bytes::new()));
                }

                socket
                    .send(AxumWsMessage::text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let subscribe_msg: ClientMessage = serde_json::from_str(&new_message).unwrap();
                assert!(matches!(subscribe_msg, ClientMessage::Subscribe { .. }));
                #[allow(unused_assignments)]
                let mut client_id = None;
                if let ClientMessage::Subscribe { payload, id } = subscribe_msg {
                    client_id = Some(id);
                    assert_eq!(
                        payload,
                        Request::builder()
                            .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                            .build()
                    );
                } else {
                    panic!("we should receive a subscribe message");
                }

                socket
                    .send(AxumWsMessage::text("coucou"))
                    .await
                    .unwrap();

                if let Some(duration) = heartbeat_interval {
                   tokio::time::pause();
                   assert!(
                       socket.next().now_or_never().is_none(),
                       "It should be no pending messages"
                   );

                   tokio::time::sleep(duration).await;
                   let ping_message = socket.next().await.unwrap().unwrap();
                   assert_eq!(ping_message, AxumWsMessage::text(
                       serde_json::to_string(&ClientMessage::Ping { payload: None }).unwrap(),
                   ));

                   assert!(
                       socket.next().now_or_never().is_none(),
                       "It should be no pending messages"
                   );
                   tokio::time::resume();
                }

                socket
                    .send(AxumWsMessage::text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id.clone().unwrap(), payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();

                socket
                    .send(AxumWsMessage::Ping(Bytes::new()))
                    .await
                    .unwrap();

                let pong_message = socket.next().await.unwrap().unwrap();
                assert_eq!(pong_message, AxumWsMessage::Pong(Bytes::new()));

                socket
                    .send(AxumWsMessage::Ping(Bytes::new()))
                    .await
                    .unwrap();

                let pong_message = socket.next().await.unwrap().unwrap();
                assert_eq!(pong_message, AxumWsMessage::Pong(Bytes::new()));

                socket
                    .send(AxumWsMessage::text(
                        serde_json::to_string(&ServerMessage::Complete { id: client_id.unwrap() }).unwrap(),
                    ))
                    .await
                    .unwrap();

                let terminate_sub = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let terminate_msg: ClientMessage = serde_json::from_str(&terminate_sub).unwrap();
                assert!(matches!(terminate_msg, ClientMessage::OldConnectionTerminate));
                socket.close().await.unwrap();
            });

            Ok::<_, Infallible>(res)
        };

        let app = Router::new().route("/ws", get(ws_handler));
        let listener =
            tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port.unwrap_or_default()))
                .await
                .unwrap();
        let server = axum::serve(listener, app);
        let local_addr = server.local_addr().unwrap();
        tokio::spawn(async { server.await.unwrap() });
        local_addr
    }

    async fn emulate_correct_websocket_server_old_protocol(
        send_ping: bool,
        port: Option<u16>,
    ) -> SocketAddr {
        let ws_handler = move |ws: WebSocketUpgrade| async move {
            let res = ws.protocols([SUBSCRIPTIONS_TRANSPORT_WS_SUBPROTOCOL]).on_upgrade(move |mut socket| async move {
                let init_connection = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let init_msg: ClientMessage = serde_json::from_str(&init_connection).unwrap();
                assert!(matches!(init_msg, ClientMessage::ConnectionInit { .. }));

                if send_ping {
                    // It turns out some servers may send Pings before they even ack the connection.
                    socket
                        .send(AxumWsMessage::Ping(Bytes::new()))
                        .await
                        .unwrap();
                    let pong_message = socket.next().await.unwrap().unwrap();
                    assert_eq!(pong_message, AxumWsMessage::Pong(Bytes::new()));
                }
                socket
                    .send(AxumWsMessage::text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(AxumWsMessage::text(
                        serde_json::to_string(&ServerMessage::KeepAlive).unwrap(),
                    ))
                    .await
                    .unwrap();
                let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let subscribe_msg: ClientMessage = serde_json::from_str(&new_message).unwrap();
                assert!(matches!(subscribe_msg, ClientMessage::OldStart { .. }));
                #[allow(unused_assignments)]
                let mut client_id = None;
                if let ClientMessage::OldStart { payload, id } = subscribe_msg {
                    client_id = Some(id);
                    assert_eq!(
                        payload,
                        Request::builder()
                            .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                            .build()
                    );
                } else {
                    panic!("we should receive a subscribe message");
                }

                socket
                    .send(AxumWsMessage::text("coucou"))
                    .await
                    .unwrap();

                socket
                    .send(AxumWsMessage::text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id.clone().unwrap(), payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(AxumWsMessage::text(
                        serde_json::to_string(&ServerMessage::KeepAlive).unwrap(),
                    ))
                    .await
                    .unwrap();

                let stop_sub = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let stop_msg: ClientMessage = serde_json::from_str(&stop_sub).unwrap();
                assert!(matches!(stop_msg, ClientMessage::OldStop { .. }));

                let terminate_sub = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let terminate_msg: ClientMessage = serde_json::from_str(&terminate_sub).unwrap();
                assert!(matches!(terminate_msg, ClientMessage::OldConnectionTerminate));

                socket.close().await.unwrap();
            });

            Ok::<_, Infallible>(res)
        };

        let app = Router::new().route("/ws", get(ws_handler));
        let listener =
            tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port.unwrap_or_default()))
                .await
                .unwrap();
        let server = axum::serve(listener, app);
        let local_addr = server.local_addr().unwrap();
        tokio::spawn(async { server.await.unwrap() });
        local_addr
    }

    #[tokio::test]
    async fn test_ws_connection_new_proto_with_ping() {
        test_ws_connection_new_proto(true, None, None).await
    }

    #[tokio::test]
    async fn test_ws_connection_new_proto_without_ping() {
        test_ws_connection_new_proto(false, None, None).await
    }

    #[tokio::test]
    async fn test_ws_connection_new_proto_with_heartbeat() {
        test_ws_connection_new_proto(false, Some(tokio::time::Duration::from_secs(60)), None).await
    }

    async fn test_ws_connection_new_proto(
        send_ping: bool,
        heartbeat_interval: Option<tokio::time::Duration>,
        port: Option<u16>,
    ) {
        let socket_addr =
            emulate_correct_websocket_server_new_protocol(send_ping, heartbeat_interval, port)
                .await;
        let url = format!("ws://{socket_addr}/ws");
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(GRAPHQL_WS_SUBPROTOCOL),
        );
        let (ws_stream, _resp) = connect_async(request).await.unwrap();

        async move {
            let sub_uuid = Uuid::new_v4();
            let gql_socket = GraphqlWebSocket::new(
                convert_websocket_stream(ws_stream, sub_uuid.to_string()),
                sub_uuid.to_string(),
                WebSocketProtocol::GraphqlWs,
                Some(serde_json_bytes::json!({
                    "token": "XXX"
                })),
            )
            .await
            .unwrap();

            let sub = "subscription {\n  userWasCreated {\n    username\n  }\n}";
            let mut gql_read_stream = gql_socket
                .into_subscription(
                    graphql::Request::builder().query(sub).build(),
                    heartbeat_interval,
                )
                .await
                .unwrap();

            // Starts at 1 for the connection ack message
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                1,
                subscriptions.mode = "passthrough"
            );

            let next_payload = gql_read_stream.next().await.unwrap();
            assert_response_eq_ignoring_error_id!(next_payload, graphql::Response::builder()
                .error(
                    graphql::Error::builder()
                        .message(
                            "cannot deserialize websocket server message: Error(\"expected value\", line: 1, column: 1)".to_string())
                        .extension_code("INVALID_WEBSOCKET_SERVER_MESSAGE_FORMAT")
                        .build(),
                )
                .build()
            );
            // Increments to 2 for the invalid message
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                2,
                subscriptions.mode = "passthrough"
            );

            let next_payload = gql_read_stream.next().await.unwrap();
            assert_eq!(
                next_payload,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );
            // Increments to 3 for the next message
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                3,
                subscriptions.mode = "passthrough"
            );

            assert!(
                gql_read_stream.next().now_or_never().is_none(),
                "It should be completed"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_ws_connection_new_proto_error_on_init() {
        let ws_handler = move |ws: WebSocketUpgrade| async move {
            let res =
                ws.protocols(["graphql-transport-ws"])
                    .on_upgrade(move |mut socket| async move {
                        let connection_ack =
                            socket.recv().await.unwrap().unwrap().into_text().unwrap();
                        let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                        if let ClientMessage::ConnectionInit { payload } = ack_msg {
                            assert_eq!(
                                payload,
                                Some(serde_json_bytes::json!({"connectionParams": {
                                    "token": "XXX"
                                }}))
                            );
                        } else {
                            panic!("it should be a connection init message");
                        }

                        socket
                            .send(AxumWsMessage::text(
                                r#"{"type": "connection_error", "payload": {"message": "PAYLOAD_MESSAGE_ERROR"}}"#,
                            ))
                            .await
                            .unwrap();

                        socket.close().await.unwrap();
                    });

            Ok::<_, Infallible>(res)
        };

        let app = Router::new().route("/ws", get(ws_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = axum::serve(listener, app);
        let socket_addr = server.local_addr().unwrap();
        tokio::spawn(async { server.await.unwrap() });

        let url = format!("ws://{socket_addr}/ws");
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("graphql-transport-ws"),
        );
        let (ws_stream, _resp) = connect_async(request).await.unwrap();

        let sub_uuid = Uuid::new_v4();
        let res = GraphqlWebSocket::new(
            convert_websocket_stream(ws_stream, sub_uuid.to_string()),
            sub_uuid.to_string(),
            WebSocketProtocol::GraphqlWs,
            Some(serde_json_bytes::json!({
                "token": "XXX"
            })),
        )
        .await;

        assert!(res.is_err());
        let err = res.err().unwrap();
        println!("err: {err:?}");
        assert!(
            err.message
                .as_str()
                .starts_with("didn't receive the connection ack from websocket connection")
        );
        assert!(
            err.message
                .as_str()
                .contains(r#"Error(Error { message: "PAYLOAD_MESSAGE_ERROR"#)
        );
        assert_eq!(err.extensions.get("code").unwrap(), "WEBSOCKET_ACK_ERROR");
    }

    #[tokio::test]
    async fn test_ws_connection_old_proto_with_ping() {
        test_ws_connection_old_proto(true, None).await
    }

    #[tokio::test]
    async fn test_ws_connection_old_proto_without_ping() {
        test_ws_connection_old_proto(false, None).await
    }

    async fn test_ws_connection_old_proto(send_ping: bool, port: Option<u16>) {
        let socket_addr = emulate_correct_websocket_server_old_protocol(send_ping, port).await;
        let url = format!("ws://{socket_addr}/ws");
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(SUBSCRIPTIONS_TRANSPORT_WS_SUBPROTOCOL),
        );
        let (ws_stream, _resp) = connect_async(request).await.unwrap();

        async move {
            let sub_uuid = Uuid::new_v4();
            let gql_socket = GraphqlWebSocket::new(
                convert_websocket_stream(ws_stream, sub_uuid.to_string()),
                sub_uuid.to_string(),
                WebSocketProtocol::SubscriptionsTransportWs,
                None,
            )
            .await
            .unwrap();

            let sub = "subscription {\n  userWasCreated {\n    username\n  }\n}";
            let mut gql_read_stream = gql_socket
                .into_subscription(graphql::Request::builder().query(sub).build(), None)
                .await
                .unwrap();

            // Starts at 1 for the connection ack
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                1,
                subscriptions.mode = "passthrough"
            );

            let next_payload = gql_read_stream.next().await.unwrap();
            assert_response_eq_ignoring_error_id!(next_payload, graphql::Response::builder()
                .error(
                    graphql::Error::builder()
                        .message(
                            "cannot deserialize websocket server message: Error(\"expected value\", line: 1, column: 1)".to_string())
                        .extension_code("INVALID_WEBSOCKET_SERVER_MESSAGE_FORMAT")
                        .build(),
                )
                .build()
            );
            // Increments to 3 for the keepalive and invalid message
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                3,
                subscriptions.mode = "passthrough"
            );

            let next_payload = gql_read_stream.next().await.unwrap();
            assert_eq!(
                next_payload,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );
            // Increments to 4 for the next message
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                4,
                subscriptions.mode = "passthrough"
            );

            assert!(
                gql_read_stream.next().now_or_never().is_none(),
                "It should be completed"
            );
        }
        .with_metrics()
        .await;
    }
}
