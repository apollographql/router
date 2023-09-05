use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use futures::stream::select;
use futures::stream::StreamExt;
use futures::Stream;
use serde::Serialize;
use serde_json_bytes::Value;
use tokio_stream::once;
use tokio_stream::wrappers::IntervalStream;

use crate::graphql;

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

enum MessageKind {
    Heartbeat,
    Message(graphql::Response),
    Eof,
}

pub(crate) struct Multipart {
    stream: Pin<Box<dyn Stream<Item = MessageKind> + Send>>,
    is_first_chunk: bool,
    is_terminated: bool,
    mode: ProtocolMode,
}

impl Multipart {
    pub(crate) fn new<S>(stream: S, mode: ProtocolMode) -> Self
    where
        S: Stream<Item = graphql::Response> + Send + 'static,
    {
        let stream = match mode {
            ProtocolMode::Subscription => select(
                stream
                    .map(MessageKind::Message)
                    .chain(once(MessageKind::Eof)),
                IntervalStream::new(tokio::time::interval(HEARTBEAT_INTERVAL))
                    .map(|_| MessageKind::Heartbeat),
            )
            .boxed(),
            ProtocolMode::Defer => stream.map(MessageKind::Message).boxed(),
        };

        Self {
            stream,
            is_first_chunk: true,
            is_terminated: false,
            mode,
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
                    let buf = if self.is_first_chunk {
                        self.is_first_chunk = false;
                        Bytes::from_static(
                            &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql\r\n"[..]
                        )
                    } else {
                        Bytes::from_static(
                            &b"content-type: application/json\r\n\r\n{}\r\n--graphql\r\n"[..],
                        )
                    };

                    Poll::Ready(Some(Ok(buf)))
                }
                Some(MessageKind::Message(mut response)) => {
                    let mut buf = if self.is_first_chunk {
                        self.is_first_chunk = false;
                        Vec::from(&b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n"[..])
                    } else {
                        Vec::from(&b"content-type: application/json\r\n\r\n"[..])
                    };
                    let is_still_open =
                        response.has_next.unwrap_or(false) || response.subscribed.unwrap_or(false);
                    match self.mode {
                        ProtocolMode::Subscription => {
                            let resp = SubscriptionPayload {
                                errors: if is_still_open {
                                    Vec::new()
                                } else {
                                    response.errors.drain(..).collect()
                                },
                                payload: match response.data {
                                    None | Some(Value::Null) => None,
                                    _ => response.into(),
                                },
                            };

                            serde_json::to_writer(&mut buf, &resp)?;
                        }
                        ProtocolMode::Defer => {
                            serde_json::to_writer(&mut buf, &response)?;
                        }
                    }

                    if is_still_open {
                        buf.extend_from_slice(b"\r\n--graphql\r\n");
                    } else {
                        self.is_terminated = true;
                        buf.extend_from_slice(b"\r\n--graphql--\r\n");
                    }

                    Poll::Ready(Some(Ok(buf.into())))
                }
                Some(MessageKind::Eof) => {
                    // If the stream ends or is empty
                    let buf = if self.is_first_chunk {
                        self.is_first_chunk = false;
                        Bytes::from_static(
                            &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{}\r\n--graphql--\r\n"[..]
                        )
                    } else {
                        Bytes::from_static(
                            &b"content-type: application/json\r\n\r\n{}\r\n--graphql--\r\n"[..],
                        )
                    };
                    self.is_terminated = true;

                    Poll::Ready(Some(Ok(buf)))
                }
                None => {
                    self.is_terminated = true;
                    Poll::Ready(None)
                }
            },
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;
    use serde_json_bytes::ByteString;

    use super::*;

    // TODO add test with empty stream

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
                .build(),
        ];
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
                        assert_eq!(res, "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"payload\":{\"data\":\"foo\"}}\r\n--graphql\r\n");
                    }
                    1 => {
                        assert_eq!(
                            res,
                            "content-type: application/json\r\n\r\n{\"payload\":{\"data\":\"bar\"}}\r\n--graphql\r\n"
                        );
                    }
                    2 => {
                        assert_eq!(
                            res,
                            "content-type: application/json\r\n\r\n{\"payload\":{\"data\":\"foobar\"}}\r\n--graphql--\r\n"
                        );
                    }
                    _ => {
                        panic!("should not happened, test failed");
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
            let res = dbg!(String::from_utf8(resp.unwrap().to_vec()).unwrap());
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
}
