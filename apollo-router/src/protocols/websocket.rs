use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use futures::future;
use futures::Future;
use futures::Sink;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use http::HeaderValue;
use pin_project_lite::pin_project;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::graphql;

const CONNECTION_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebSocketProtocol {
    // New one
    GraphqlWs,
    #[serde(rename = "graphql_transport_ws")]
    // Old one
    SubscriptionsTransportWs,
}

impl Default for WebSocketProtocol {
    fn default() -> Self {
        Self::GraphqlWs
    }
}

impl From<WebSocketProtocol> for HeaderValue {
    fn from(value: WebSocketProtocol) -> Self {
        match value {
            WebSocketProtocol::GraphqlWs => HeaderValue::from_static("graphql-transport-ws"),
            WebSocketProtocol::SubscriptionsTransportWs => HeaderValue::from_static("graphql-ws"),
        }
    }
}

impl WebSocketProtocol {
    fn subscribe(&self, id: String, payload: graphql::Request) -> ClientMessage {
        match self {
            // old
            WebSocketProtocol::SubscriptionsTransportWs => ClientMessage::OldStart { id, payload },
            // new
            WebSocketProtocol::GraphqlWs => ClientMessage::Subscribe { id, payload },
        }
    }

    fn complete(&self, id: String) -> ClientMessage {
        match self {
            // old
            WebSocketProtocol::SubscriptionsTransportWs => ClientMessage::OldStop { id },
            // new
            WebSocketProtocol::GraphqlWs => ClientMessage::Complete { id },
        }
    }
}

/// A websocket message received from the client
#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Request is at fault
pub(crate) enum ClientMessage {
    /// A new connection
    ConnectionInit {
        /// Optional init payload from the client
        payload: Option<serde_json_bytes::Value>,
    },
    /// The start of a Websocket subscription
    Subscribe {
        /// Message ID
        id: String,
        /// The GraphQL Request - this can be modified by protocol implementors
        /// to add files uploads.
        payload: graphql::Request,
    },
    #[serde(rename = "start")]
    /// For old protocol
    OldStart {
        /// Message ID
        id: String,
        /// The GraphQL Request - this can be modified by protocol implementors
        /// to add files uploads.
        payload: graphql::Request,
    },
    /// The end of a Websocket subscription
    Complete {
        /// Message ID
        id: String,
    },
    /// For old protocol
    #[serde(rename = "stop")]
    OldStop {
        /// Message ID
        id: String,
    },
    /// Connection terminated by the client
    ConnectionTerminate,
    /// Useful for detecting failed connections, displaying latency metrics or
    /// other types of network probing.
    ///
    /// Reference: <https://github.com/enisdenjo/graphql-ws/blob/master/PROTOCOL.md#ping>
    Ping {
        /// Additional details about the ping.
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json_bytes::Value>,
    },
    /// The response to the Ping message.
    ///
    /// Reference: <https://github.com/enisdenjo/graphql-ws/blob/master/PROTOCOL.md#pong>
    Pong {
        /// Additional details about the pong.
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json_bytes::Value>,
    },
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerMessage {
    ConnectionAck,
    /// subscriptions-transport-ws protocol alias for next payload
    #[serde(alias = "data")]
    /// graphql-ws protocol next payload
    Next {
        id: String,
        payload: graphql::Response,
    },
    #[serde(alias = "connection_error")]
    Error {
        id: String,
        payload: ServerError,
    },
    Complete {
        id: String,
    },
    #[serde(alias = "ka")]
    KeepAlive,
    /// The response to the Ping message.
    ///
    /// https://github.com/enisdenjo/graphql-ws/blob/master/PROTOCOL.md#pong
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
            ServerMessage::Next { id, .. }
            | ServerMessage::Error { id, .. }
            | ServerMessage::Complete { id } => Some(id.to_string()),
        }
    }
}

pin_project! {
pub(crate) struct GraphqlWebSocket<S> {
    #[pin]
    stream: S,
    id: String,
    protocol: WebSocketProtocol,
    // Booleans for state machine when closing the stream
    completed: bool,
    terminated: bool,
}
}

impl<S> GraphqlWebSocket<S>
where
    S: Stream<Item = serde_json::Result<ServerMessage>> + Sink<ClientMessage> + std::marker::Unpin,
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
                    Some(Ok(ServerMessage::Ping { payload })) => {
                        // we don't mind an error here
                        // because it will fall through the error below
                        // if we haven't been able to properly get a ConnectionAck within the `CONNECTION_ACK_TIMEOUT`
                        let _ = stream
                            .send(ClientMessage::Pong {
                                payload: payload.map(|p| p.into()),
                            })
                            .await;
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
                .message("didn't receive the connection ack from websocket connection")
                .extension_code("WEBSOCKET_ACK_ERROR")
                .build());
        }

        Ok(Self {
            stream,
            id,
            protocol,
            completed: false,
            terminated: false,
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

pub(crate) fn convert_websocket_stream<T>(
    stream: WebSocketStream<T>,
    id: String,
) -> impl Stream<Item = serde_json::Result<ServerMessage>> + Sink<ClientMessage, Error = Error>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .with(|client_message: ClientMessage| {
            // It applies to the Sink
            future::ready(match serde_json::to_string(&client_message) {
                Ok(client_message_str) => Ok(Message::Text(client_message_str)),
                Err(err) => Err(Error::SerdeError(err)),
            })
        })
        .map(move |msg| match msg {
            // It applies to the Stream
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
                        id: id.to_string(),
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
                tracing::error!("cannot consume more message on websocket stream: {err:?}");

                Ok(ServerMessage::Error {
                    id: id.to_string(),
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

impl<S> Stream for GraphqlWebSocket<S>
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
                        if let Some(id) = &server_message.id() {
                            if this.id != id {
                                tracing::error!("we should not receive data from other subscriptions, closing the stream");
                                return Poll::Ready(None);
                            }
                        }
                        if let ServerMessage::Ping { .. } = server_message {
                            // Send pong asynchronously
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

impl<S> Sink<graphql::Request> for GraphqlWebSocket<S>
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

    fn start_send(self: Pin<&mut Self>, item: graphql::Request) -> Result<(), Self::Error> {
        let mut this = self.project();

        tracing::info!(
            monotonic_counter
                .apollo
                .router
                .operations
                .subscriptions
                .events = 1u64,
            subscriptions.mode = "passthrough"
        );
        Pin::new(&mut this.stream)
            .start_send(this.protocol.subscribe(this.id.to_string(), item))
            .map_err(|_err| {
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
        tracing::info!(
            monotonic_counter
                .apollo
                .router
                .operations
                .subscriptions
                .events = 1u64,
            subscriptions.mode = "passthrough",
            subscriptions.complete = true
        );
        let mut this = self.project();
        if !*this.completed {
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
        if let WebSocketProtocol::SubscriptionsTransportWs = this.protocol {
            if !*this.terminated {
                match Pin::new(
                    &mut Pin::new(&mut this.stream).send(ClientMessage::ConnectionTerminate),
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
        }
        Pin::new(&mut this.stream).poll_close(cx).map_err(|_err| {
            graphql::Error::builder()
                .message("cannot close websocket connection")
                .extension_code("WEBSOCKET_CONNECTION_ERROR")
                .build()
        })
    }
}

#[derive(Deserialize, Serialize)]
struct WithId {
    id: String,
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;

    use axum::extract::ws::Message as AxumWsMessage;
    use axum::extract::WebSocketUpgrade;
    use axum::routing::get;
    use axum::Router;
    use axum::Server;
    use futures::StreamExt;
    use http::HeaderValue;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use uuid::Uuid;

    use super::*;
    use crate::graphql::Request;

    async fn emulate_correct_websocket_server_new_protocol(
        send_ping: bool,
        port: Option<u16>,
    ) -> SocketAddr {
        let ws_handler = move |ws: WebSocketUpgrade| async move {
            let res = ws.on_upgrade(move |mut socket| async move {
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
                        .send(AxumWsMessage::Text(
                            serde_json::to_string(&ServerMessage::Ping { payload: None }).unwrap(),
                        ))
                        .await
                        .unwrap();
                    let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    let pong_message: ClientMessage = serde_json::from_str(&new_message).unwrap();
                    assert!(matches!(pong_message, ClientMessage::Pong { payload: None }));
                }

                socket
                    .send(AxumWsMessage::Text(
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
                    .send(AxumWsMessage::Text(
                        "coucou".to_string(),
                    ))
                    .await
                    .unwrap();

                socket
                    .send(AxumWsMessage::Text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id.clone().unwrap(), payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();

                socket
                    .send(AxumWsMessage::Text(
                        serde_json::to_string(&ServerMessage::Ping { payload: None }).unwrap(),
                    ))
                    .await
                    .unwrap();

                let pong_message = socket.next().await.unwrap().unwrap();
                assert_eq!(pong_message, AxumWsMessage::Text(
                    serde_json::to_string(&ClientMessage::Pong { payload: None }).unwrap(),
                ));

                socket
                    .send(AxumWsMessage::Text(
                        serde_json::to_string(&ServerMessage::Ping { payload: None }).unwrap(),
                    ))
                    .await
                    .unwrap();

                let pong_message = socket.next().await.unwrap().unwrap();
                assert_eq!(pong_message, AxumWsMessage::Text(
                    serde_json::to_string(&ClientMessage::Pong { payload: None }).unwrap(),
                ));

                socket
                    .send(AxumWsMessage::Text(
                        serde_json::to_string(&ServerMessage::Complete { id: client_id.unwrap() }).unwrap(),
                    ))
                    .await
                    .unwrap();

                let terminate_sub = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let terminate_msg: ClientMessage = serde_json::from_str(&terminate_sub).unwrap();
                assert!(matches!(terminate_msg, ClientMessage::ConnectionTerminate));
                socket.close().await.unwrap();
            });

            Ok::<_, Infallible>(res)
        };

        let app = Router::new().route("/ws", get(ws_handler));
        let server = Server::bind(
            &format!("127.0.0.1:{}", port.unwrap_or_default())
                .parse()
                .unwrap(),
        )
        .serve(app.into_make_service());
        let local_addr = server.local_addr();
        tokio::spawn(async { server.await.unwrap() });
        local_addr
    }

    async fn emulate_correct_websocket_server_old_protocol(
        send_ping: bool,
        port: Option<u16>,
    ) -> SocketAddr {
        let ws_handler = move |ws: WebSocketUpgrade| async move {
            let res = ws.on_upgrade(move |mut socket| async move {
                let init_connection = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let init_msg: ClientMessage = serde_json::from_str(&init_connection).unwrap();
                assert!(matches!(init_msg, ClientMessage::ConnectionInit { .. }));

                if send_ping {
                    // It turns out some servers may send Pings before they even ack the connection.
                    socket
                        .send(AxumWsMessage::Text(
                            serde_json::to_string(&ServerMessage::Ping { payload: None }).unwrap(),
                        ))
                        .await
                        .unwrap();
                    let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    let pong_message: ClientMessage = serde_json::from_str(&new_message).unwrap();
                    assert!(matches!(pong_message, ClientMessage::Pong { payload: None }));
                }
                socket
                    .send(AxumWsMessage::Text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(AxumWsMessage::Text(
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
                    .send(AxumWsMessage::Text(
                        "coucou".to_string(),
                    ))
                    .await
                    .unwrap();

                socket
                    .send(AxumWsMessage::Text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id.clone().unwrap(), payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(AxumWsMessage::Text(
                        serde_json::to_string(&ServerMessage::KeepAlive).unwrap(),
                    ))
                    .await
                    .unwrap();

                let stop_sub = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let stop_msg: ClientMessage = serde_json::from_str(&stop_sub).unwrap();
                assert!(matches!(stop_msg, ClientMessage::OldStop { .. }));

                let terminate_sub = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let terminate_msg: ClientMessage = serde_json::from_str(&terminate_sub).unwrap();
                assert!(matches!(terminate_msg, ClientMessage::ConnectionTerminate));

                socket.close().await.unwrap();
            });

            Ok::<_, Infallible>(res)
        };

        let app = Router::new().route("/ws", get(ws_handler));
        let server = Server::bind(
            &format!("127.0.0.1:{}", port.unwrap_or_default())
                .parse()
                .unwrap(),
        )
        .serve(app.into_make_service());
        let local_addr = server.local_addr();
        tokio::spawn(async { server.await.unwrap() });
        local_addr
    }

    #[tokio::test]
    async fn test_ws_connection_new_proto_with_ping() {
        test_ws_connection_new_proto(true, None).await
    }

    #[tokio::test]
    async fn test_ws_connection_new_proto_without_ping() {
        test_ws_connection_new_proto(false, None).await
    }

    async fn test_ws_connection_new_proto(send_ping: bool, port: Option<u16>) {
        let socket_addr = emulate_correct_websocket_server_new_protocol(send_ping, port).await;
        let url = url::Url::parse(format!("ws://{}/ws", socket_addr).as_str()).unwrap();
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("graphql-transport-ws"),
        );
        let (ws_stream, _resp) = connect_async(request).await.unwrap();

        let sub_uuid = Uuid::new_v4();
        let gql_stream = GraphqlWebSocket::new(
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
        let (mut gql_sink, mut gql_read_stream) = gql_stream.split();
        let _handle = tokio::task::spawn(async move {
            gql_sink
                .send(graphql::Request::builder().query(sub).build())
                .await
                .unwrap();
        });

        let next_payload = gql_read_stream.next().await.unwrap();
        assert_eq!(next_payload, graphql::Response::builder()
            .error(
                graphql::Error::builder()
                    .message(
                        "cannot deserialize websocket server message: Error(\"expected value\", line: 1, column: 1)".to_string())
                    .extension_code("INVALID_WEBSOCKET_SERVER_MESSAGE_FORMAT")
                    .build(),
            )
            .build()
        );

        let next_payload = gql_read_stream.next().await.unwrap();
        assert_eq!(
            next_payload,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );
        assert!(
            gql_read_stream.next().await.is_none(),
            "It should be completed"
        );
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
        let url = url::Url::parse(format!("ws://{}/ws", socket_addr).as_str()).unwrap();
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("graphql-ws"),
        );
        let (ws_stream, _resp) = connect_async(request).await.unwrap();

        let sub_uuid = Uuid::new_v4();
        let gql_stream = GraphqlWebSocket::new(
            convert_websocket_stream(ws_stream, sub_uuid.to_string()),
            sub_uuid.to_string(),
            WebSocketProtocol::SubscriptionsTransportWs,
            None,
        )
        .await
        .unwrap();

        let sub = "subscription {\n  userWasCreated {\n    username\n  }\n}";
        let (mut gql_sink, mut gql_read_stream) = gql_stream.split();
        let _handle = tokio::task::spawn(async move {
            gql_sink
                .send(graphql::Request::builder().query(sub).build())
                .await
                .unwrap();
            gql_sink.close().await.unwrap();
        });

        let next_payload = gql_read_stream.next().await.unwrap();
        assert_eq!(next_payload, graphql::Response::builder()
            .error(
                graphql::Error::builder()
                    .message(
                        "cannot deserialize websocket server message: Error(\"expected value\", line: 1, column: 1)".to_string())
                    .extension_code("INVALID_WEBSOCKET_SERVER_MESSAGE_FORMAT")
                    .build(),
            )
            .build()
        );

        let next_payload = gql_read_stream.next().await.unwrap();
        assert_eq!(
            next_payload,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );
        assert!(
            gql_read_stream.next().await.is_none(),
            "It should be completed"
        );
    }
}
