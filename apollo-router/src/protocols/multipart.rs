use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use futures::stream::StreamExt;
use futures::stream::select;
use serde::Serialize;
use serde_json_bytes::Value;
use tokio_stream::once;
use tokio_stream::wrappers::IntervalStream;
use tracing::Span;

use crate::graphql;
use crate::plugins::subscription::SUBSCRIPTION_ERROR_EXTENSION_KEY;

#[cfg(test)]
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(not(test))]
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("serialization error")]
    SerdeError(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ProtocolMode {
    Subscription,
    Defer,
}

#[derive(Clone, Debug, Serialize)]
struct SubscriptionPayload {
    payload: Option<graphql::Response>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<graphql::Error>,
}

#[derive(Debug)]
enum MessageKind {
    Heartbeat,
    Message(Box<graphql::Response>),
    Eof,
}

pub(crate) struct Multipart {
    stream: Pin<Box<dyn Stream<Item = MessageKind> + Send>>,
    is_first_chunk: bool,
    is_terminated: bool,
    mode: ProtocolMode,
    /// Tracks whether a heartbeat was sent but not yet followed by another poll.
    /// Used to detect if a heartbeat was the last thing sent before connection closed.
    heartbeat_pending: bool,
    /// The span captured at creation time, used to record attributes on connection close.
    span: Span,
}

impl Multipart {
    pub(crate) fn new<S>(stream: S, mode: ProtocolMode) -> Self
    where
        S: Stream<Item = graphql::Response> + Send + 'static,
    {
        let stream = stream.map(|message| MessageKind::Message(Box::new(message)));
        let stream = match mode {
            ProtocolMode::Subscription => select(
                stream.chain(once(MessageKind::Eof)),
                IntervalStream::new(tokio::time::interval(HEARTBEAT_INTERVAL))
                    .map(|_| MessageKind::Heartbeat),
            )
            .boxed(),
            ProtocolMode::Defer => stream.boxed(),
        };

        Self {
            stream,
            is_first_chunk: true,
            is_terminated: false,
            mode,
            heartbeat_pending: false,
            // Capture the current span so we can record attributes later
            span: Span::current(),
        }
    }
}

/// Reasons why a subscription ended
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubscriptionEndReason {
    /// Subscription completed normally after receiving all data
    Complete,
    /// Server closed the connection gracefully
    ServerClose,
    /// Stream source ended (e.g., subgraph closed the connection)
    StreamEnd,
    /// Heartbeat could not be delivered - client likely disconnected
    HeartbeatDeliveryFailed,
    /// Client disconnected unexpectedly (after a message was sent)
    ClientDisconnect,
}

impl SubscriptionEndReason {
    /// Returns the string representation of the end reason
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ServerClose => "server_close",
            Self::StreamEnd => "stream_end",
            Self::HeartbeatDeliveryFailed => "heartbeat_delivery_failed",
            Self::ClientDisconnect => "client_disconnect",
        }
    }
}

impl Drop for Multipart {
    fn drop(&mut self) {
        // Only handle subscription mode
        if self.mode == ProtocolMode::Subscription && !self.is_terminated {
            // Stream wasn't terminated properly - determine the reason
            let reason = if self.heartbeat_pending {
                // Heartbeat was the last thing sent - likely failed to deliver
                SubscriptionEndReason::HeartbeatDeliveryFailed
            } else {
                // Connection closed after a message was sent
                SubscriptionEndReason::ClientDisconnect
            };
            self.span
                .record("apollo.subscription.end_reason", reason.as_str());
        }
    }
}

impl Stream for Multipart {
    type Item = Result<Bytes, Error>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if self.is_terminated {
            return Poll::Ready(None);
        }
        match self.stream.as_mut().poll_next(cx) {
            Poll::Ready(message) => match message {
                Some(MessageKind::Heartbeat) => {
                    // It's the ticker for heartbeat for subscription
                    // Mark that we're sending a heartbeat - if the stream is dropped before
                    // the next poll, we know the heartbeat delivery likely failed
                    self.heartbeat_pending = true;

                    let buf = if self.is_first_chunk {
                        self.is_first_chunk = false;
                        Bytes::from_static(
                            &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql"[..]
                        )
                    } else {
                        Bytes::from_static(
                            &b"\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql"[..],
                        )
                    };

                    Poll::Ready(Some(Ok(buf)))
                }
                Some(MessageKind::Message(mut response)) => {
                    // Clear heartbeat pending flag since we received a message poll
                    self.heartbeat_pending = false;

                    let is_still_open =
                        response.has_next.unwrap_or(false) || response.subscribed.unwrap_or(false);
                    let mut buf = if self.is_first_chunk {
                        self.is_first_chunk = false;
                        Vec::from(&b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n"[..])
                    } else {
                        Vec::from(&b"\r\ncontent-type: application/json\r\n\r\n"[..])
                    };

                    match self.mode {
                        ProtocolMode::Subscription => {
                            let is_transport_error =
                                response.extensions.remove(SUBSCRIPTION_ERROR_EXTENSION_KEY)
                                    == Some(true.into());
                            // Magic empty response (that we create internally) means the connection was gracefully closed at the server side
                            if !is_still_open
                                && response.data.is_none()
                                && response.errors.is_empty()
                                && response.extensions.is_empty()
                            {
                                self.is_terminated = true;
                                self.span.record(
                                    "apollo.subscription.end_reason",
                                    SubscriptionEndReason::ServerClose.as_str(),
                                );
                                return Poll::Ready(Some(Ok(Bytes::from_static(&b"--\r\n"[..]))));
                            }

                            let response = if is_transport_error {
                                SubscriptionPayload {
                                    errors: std::mem::take(&mut response.errors),
                                    payload: match response.data {
                                        None | Some(Value::Null)
                                            if response.extensions.is_empty() =>
                                        {
                                            None
                                        }
                                        _ => (*response).into(),
                                    },
                                }
                            } else {
                                SubscriptionPayload {
                                    errors: Vec::new(),
                                    payload: (*response).into(),
                                }
                            };

                            serde_json::to_writer(&mut buf, &response)?;
                        }
                        ProtocolMode::Defer => {
                            serde_json::to_writer(&mut buf, &response)?;
                        }
                    }

                    if is_still_open {
                        buf.extend_from_slice(b"\r\n--graphql");
                    } else {
                        self.is_terminated = true;
                        if self.mode == ProtocolMode::Subscription {
                            self.span.record(
                                "apollo.subscription.end_reason",
                                SubscriptionEndReason::Complete.as_str(),
                            );
                        }
                        buf.extend_from_slice(b"\r\n--graphql--\r\n");
                    }

                    Poll::Ready(Some(Ok(buf.into())))
                }
                Some(MessageKind::Eof) => {
                    // If the stream ends or is empty - this is a clean termination
                    self.heartbeat_pending = false;
                    let buf = if self.is_first_chunk {
                        self.is_first_chunk = false;
                        Bytes::from_static(
                            &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql--\r\n"[..]
                        )
                    } else {
                        Bytes::from_static(
                            &b"\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql--\r\n"[..],
                        )
                    };
                    self.is_terminated = true;
                    self.span.record(
                        "apollo.subscription.end_reason",
                        SubscriptionEndReason::StreamEnd.as_str(),
                    );

                    Poll::Ready(Some(Ok(buf)))
                }
                None => {
                    // Stream ended - this is a clean termination
                    self.heartbeat_pending = false;
                    self.is_terminated = true;
                    self.span.record(
                        "apollo.subscription.end_reason",
                        SubscriptionEndReason::StreamEnd.as_str(),
                    );
                    Poll::Ready(None)
                }
            },
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::stream;
    use serde_json_bytes::ByteString;
    use tracing::Id;
    use tracing::Subscriber;
    use tracing::field::Visit;
    use tracing::span::Record;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// A test layer that captures the recorded `apollo.subscription.end_reason` value
    #[derive(Clone)]
    struct EndReasonCapture {
        captured_reason: Arc<std::sync::Mutex<Option<String>>>,
    }

    struct EndReasonVisitor<'a> {
        captured_reason: &'a Arc<std::sync::Mutex<Option<String>>>,
    }

    impl Visit for EndReasonVisitor<'_> {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "apollo.subscription.end_reason" {
                *self.captured_reason.lock().unwrap() = Some(value.to_string());
            }
        }

        fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
    }

    impl<S: Subscriber> Layer<S> for EndReasonCapture {
        fn on_record(&self, _span: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
            let mut visitor = EndReasonVisitor {
                captured_reason: &self.captured_reason,
            };
            values.record(&mut visitor);
        }
    }

    /// Helper to run a test with a span that has the end_reason field
    fn setup_tracing() -> (tracing::subscriber::DefaultGuard, EndReasonCapture) {
        let layer = EndReasonCapture {
            captured_reason: Arc::new(std::sync::Mutex::new(None)),
        };
        let subscriber = tracing_subscriber::registry().with(layer.clone());
        let guard = tracing::subscriber::set_default(subscriber);
        (guard, layer)
    }

    #[tokio::test]
    async fn test_end_reason_complete() {
        // Test: Subscription completes normally with subscribed=false
        let (_guard, layer) = setup_tracing();
        let span = tracing::info_span!(
            "test_span",
            "apollo.subscription.end_reason" = tracing::field::Empty
        );
        let _span_guard = span.enter();
        let responses = vec![
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from("data")))
                .subscribed(true)
                .build(),
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from("final")))
                .subscribed(false) // This marks completion
                .build(),
        ];
        let gql_responses = stream::iter(responses);
        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);

        // Consume all messages
        while protocol.next().await.is_some() {}

        let reason = layer.captured_reason.lock().unwrap().clone();

        assert_eq!(
            reason,
            Some(SubscriptionEndReason::Complete.as_str().to_string())
        );
    }

    #[tokio::test]
    async fn test_end_reason_server_close() {
        // Test: Server closes connection gracefully (empty response)
        let (_guard, layer) = setup_tracing();
        let span = tracing::info_span!(
            "test_span",
            "apollo.subscription.end_reason" = tracing::field::Empty
        );
        let _span_guard = span.enter();
        let responses = vec![
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from("data")))
                .subscribed(true)
                .build(),
            // Empty response signals server-side graceful close
            graphql::Response::builder().build(),
        ];
        let gql_responses = stream::iter(responses);
        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);

        // Consume all messages
        while protocol.next().await.is_some() {}
        let reason = layer.captured_reason.lock().unwrap().clone();
        assert_eq!(
            reason,
            Some(SubscriptionEndReason::ServerClose.as_str().to_string())
        );
    }

    #[tokio::test]
    async fn test_end_reason_stream_end() {
        // Test: Stream ends via EOF (empty stream)
        let (_guard, layer) = setup_tracing();
        let span = tracing::info_span!(
            "test_span",
            "apollo.subscription.end_reason" = tracing::field::Empty
        );
        let _span_guard = span.enter();
        let responses: Vec<graphql::Response> = vec![];
        let gql_responses = stream::iter(responses);
        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);

        // Consume all messages (will get EOF)
        while protocol.next().await.is_some() {}
        let reason = layer.captured_reason.lock().unwrap().clone();
        assert_eq!(
            reason,
            Some(SubscriptionEndReason::StreamEnd.as_str().to_string())
        );
    }

    #[tokio::test]
    async fn test_end_reason_heartbeat_delivery_failed() {
        // Test: Stream dropped while heartbeat was pending
        let (_guard, layer) = setup_tracing();
        let span = tracing::info_span!(
            "test_span",
            "apollo.subscription.end_reason" = tracing::field::Empty
        );
        let _span_guard = span.enter();
        use tokio::time::sleep;

        let (tx, rx) = tokio::sync::mpsc::channel::<graphql::Response>(1);
        let gql_responses = tokio_stream::wrappers::ReceiverStream::new(rx);
        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);

        // Spawn a task that never sends anything, then drops the sender
        tokio::spawn(async move {
            sleep(std::time::Duration::from_millis(100)).await;
            drop(tx);
        });

        // Wait for a heartbeat to be sent
        let heartbeat = "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql";
        while let Some(resp) = protocol.next().await {
            let res = String::from_utf8(resp.unwrap().to_vec()).unwrap();
            if res == heartbeat || res.starts_with("\r\ncontent-type: application/json\r\n\r\n{}") {
                // Got a heartbeat, now drop the protocol while heartbeat is pending
                assert!(protocol.heartbeat_pending);
                break;
            }
        }
        // Protocol is dropped here with heartbeat_pending = true
        drop(protocol);
        let reason = layer.captured_reason.lock().unwrap().clone();

        assert_eq!(
            reason,
            Some(
                SubscriptionEndReason::HeartbeatDeliveryFailed
                    .as_str()
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn test_end_reason_client_disconnect() {
        // Test: Stream dropped after a message (not heartbeat) was sent
        let (_guard, layer) = setup_tracing();
        let span = tracing::info_span!(
            "test_span",
            "apollo.subscription.end_reason" = tracing::field::Empty
        );
        let _span_guard = span.enter();
        let responses = vec![
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from("data")))
                .subscribed(true)
                .build(),
        ];
        let gql_responses = stream::iter(responses);
        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);

        // Get the first message
        let resp = protocol.next().await;
        assert!(resp.is_some());

        // Verify heartbeat_pending is false (we got a message, not heartbeat)
        assert!(!protocol.heartbeat_pending);

        // Protocol is dropped here without being terminated
        // and heartbeat_pending = false
        drop(protocol);
        let reason = layer.captured_reason.lock().unwrap().clone();
        assert_eq!(
            reason,
            Some(SubscriptionEndReason::ClientDisconnect.as_str().to_string())
        );
    }

    #[tokio::test]
    async fn test_end_reason_not_set_for_defer_mode() {
        // Test: Defer mode should not record any end reason
        let (_guard, layer) = setup_tracing();
        let span = tracing::info_span!(
            "test_span",
            "apollo.subscription.end_reason" = tracing::field::Empty
        );
        let _span_guard = span.enter();
        let responses = vec![
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from("data")))
                .has_next(false)
                .build(),
        ];
        let gql_responses = stream::iter(responses);
        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Defer);

        // Consume all messages
        while protocol.next().await.is_some() {}

        let reason = layer.captured_reason.lock().unwrap().clone();
        // Defer mode doesn't set end_reason
        assert_eq!(reason, None);
    }

    #[tokio::test]
    async fn test_heartbeat_and_boundaries() {
        let responses = vec![
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from(
                    String::from("foo"),
                )))
                .subscribed(true)
                .build(),
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from(
                    String::from("bar"),
                )))
                .subscribed(true)
                .build(),
            graphql::Response::builder()
                .data(serde_json_bytes::Value::String(ByteString::from(
                    String::from("foobar"),
                )))
                .subscribed(true)
                .build(),
            graphql::Response::builder()
                .data(serde_json_bytes::Value::Null)
                .extension(
                    "test",
                    serde_json_bytes::Value::String("test_extension".into()),
                )
                .subscribed(true)
                .build(),
            graphql::Response::builder().build(),
        ];
        let gql_responses = stream::iter(responses);

        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);
        let heartbeat =
            String::from("\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql");
        let mut curr_index = 0;
        while let Some(resp) = protocol.next().await {
            let res = String::from_utf8(resp.unwrap().to_vec()).unwrap();
            if res == heartbeat {
                continue;
            } else {
                match curr_index {
                    0 => {
                        assert_eq!(
                            res,
                            "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"payload\":{\"data\":\"foo\"}}\r\n--graphql"
                        );
                    }
                    1 => {
                        assert_eq!(
                            res,
                            "\r\ncontent-type: application/json\r\n\r\n{\"payload\":{\"data\":\"bar\"}}\r\n--graphql"
                        );
                    }
                    2 => {
                        assert_eq!(
                            res,
                            "\r\ncontent-type: application/json\r\n\r\n{\"payload\":{\"data\":\"foobar\"}}\r\n--graphql"
                        );
                    }
                    3 => {
                        assert_eq!(
                            res,
                            "\r\ncontent-type: application/json\r\n\r\n{\"payload\":{\"data\":null,\"extensions\":{\"test\":\"test_extension\"}}}\r\n--graphql"
                        );
                    }
                    4 => {
                        assert_eq!(res, "--\r\n");
                    }
                    _ => {
                        panic!("should not happen, test failed");
                    }
                }
                curr_index += 1;
            }
        }
    }

    #[tokio::test]
    async fn test_empty_stream() {
        let responses = vec![];
        let gql_responses = stream::iter(responses);

        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);
        let heartbeat = String::from(
            "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql\r\n",
        );
        let mut curr_index = 0;
        while let Some(resp) = protocol.next().await {
            let res = String::from_utf8(resp.unwrap().to_vec()).unwrap();
            if res == heartbeat {
                continue;
            } else {
                match curr_index {
                    0 => {
                        assert_eq!(
                            res,
                            "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql--\r\n"
                        );
                    }
                    _ => {
                        panic!("should not happen, test failed");
                    }
                }
                curr_index += 1;
            }
        }
    }

    #[tokio::test]
    async fn test_heartbeat_pending_flag() {
        use tokio::time::sleep;

        // Create a subscription stream that will have a delay to allow heartbeats
        let (tx, rx) = tokio::sync::mpsc::channel::<graphql::Response>(1);
        let gql_responses = tokio_stream::wrappers::ReceiverStream::new(rx);

        let mut protocol = Multipart::new(gql_responses, ProtocolMode::Subscription);
        let heartbeat =
            String::from("\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql");

        // Spawn a task to send a response after a delay (longer than heartbeat interval)
        tokio::spawn(async move {
            // Wait longer than the test heartbeat interval (10ms)
            sleep(std::time::Duration::from_millis(30)).await;
            let _ = tx
                .send(
                    graphql::Response::builder()
                        .data(serde_json_bytes::Value::String(ByteString::from(
                            String::from("test"),
                        )))
                        .subscribed(false)
                        .build(),
                )
                .await;
        });

        // Read items from the stream
        let mut got_heartbeat = false;
        let mut got_message = false;
        while let Some(resp) = protocol.next().await {
            let res = String::from_utf8(resp.unwrap().to_vec()).unwrap();
            if res == heartbeat || res.starts_with("\r\ncontent-type: application/json\r\n\r\n{}") {
                // After receiving a heartbeat, heartbeat_pending should be true
                assert!(
                    protocol.heartbeat_pending,
                    "heartbeat_pending should be true after yielding heartbeat"
                );
                got_heartbeat = true;
            } else if res.contains("\"test\"") {
                // After receiving a message, heartbeat_pending should be false
                assert!(
                    !protocol.heartbeat_pending,
                    "heartbeat_pending should be false after receiving message"
                );
                got_message = true;
                break;
            }
        }
        assert!(got_heartbeat, "should have received at least one heartbeat");
        assert!(got_message, "should have received the test message");
    }

    #[test]
    fn test_defer_mode_no_drop_logging() {
        // Defer mode should not trigger any logging on drop
        // This test just verifies it doesn't panic
        let responses: Vec<graphql::Response> = vec![];
        let gql_responses = stream::iter(responses);
        let protocol = Multipart::new(gql_responses, ProtocolMode::Defer);
        drop(protocol);
        // No panic = success (defer mode doesn't log on drop)
    }
}
