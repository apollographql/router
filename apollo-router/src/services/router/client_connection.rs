//! Whether the client that sent a request is still there to receive the response.
//!
//! The router learns that a client left from two signals on the client's own connection: the
//! request body fails to arrive, or hyper drops the request before it is answered. Both set the
//! request's [`ClientConnection`], and [`apply_client_connection_status`] turns whatever the
//! pipeline returned into the status that describes what happened. Errors the router gets from
//! its own outbound calls never touch this state, so they keep their usual status.

use std::error::Error as StdError;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::task::ready;

use http::StatusCode;
use http_body::Frame;
use http_body::SizeHint;
use pin_project_lite::pin_project;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::Context;
use crate::graphql;
use crate::services::router;

/// 499 Client Closed Request. HTTP does not define it, but it is the common status for a request
/// whose client left before the response was ready.
pub(crate) fn client_closed_request() -> StatusCode {
    StatusCode::from_u16(499).expect("499 is in the valid status code range")
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum State {
    Connected = 0,
    Closed,
    MalformedBody,
}

/// The state of the client connection a request arrived on.
///
/// It is stored in the request's [`Context`], and every clone shares the same state, so work that
/// carries on after the client leaves can see that nobody is waiting for its answer. A request
/// whose context has none is treated as connected.
#[derive(Clone, Debug, Default)]
pub(crate) struct ClientConnection {
    state: Arc<AtomicU8>,
}

impl ClientConnection {
    /// Records that the client closed the connection before it was answered.
    ///
    /// This takes precedence over a malformed body: a client that sent one and then left is
    /// still gone.
    pub(crate) fn close(&self) {
        self.state.store(State::Closed as u8, Ordering::Release);
    }

    /// Returns `true` once the client has closed the connection.
    pub(crate) fn is_closed(&self) -> bool {
        self.state() == State::Closed
    }

    fn reject_body(&self) {
        // A connection that already closed stays closed.
        let _ = self.state.compare_exchange(
            State::Connected as u8,
            State::MalformedBody as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn state(&self) -> State {
        match self.state.load(Ordering::Acquire) {
            0 => State::Connected,
            1 => State::Closed,
            _ => State::MalformedBody,
        }
    }

    fn record_body_error(&self, error: &(dyn StdError + 'static)) {
        match BodyError::classify(error) {
            Some(BodyError::Disconnected) => self.close(),
            Some(BodyError::Malformed) => {
                tracing::debug!(%error, "the client sent a malformed request body");
                self.reject_body();
            }
            None => {}
        }
    }
}

/// Why hyper could not deliver the client's request body.
#[derive(Debug, PartialEq)]
enum BodyError {
    /// The client went away: the connection closed or reset, or it canceled the HTTP/2 stream.
    Disconnected,
    /// The client is still there, but what it sent is not a valid HTTP/1 body.
    Malformed,
}

impl BodyError {
    /// Classifies an error read from hyper's request body. Only call this with such errors: the
    /// same IO and HTTP/2 errors from an outbound call mean something else entirely.
    fn classify(error: &(dyn StdError + 'static)) -> Option<Self> {
        let mut from_hyper = false;
        let mut cause = Some(error);
        while let Some(error) = cause {
            if let Some(error) = error.downcast_ref::<io::Error>() {
                return Some(Self::from_io(error));
            }
            // Every HTTP/2 body error ends the stream: the client reset it or sent GOAWAY, the
            // connection closed, or h2 reset it because the client broke the protocol. hyper then
            // drops the request, and no response can reach the client on that stream.
            if error.is::<h2::Error>() {
                return Some(Self::Disconnected);
            }
            from_hyper |= error.is::<hyper::Error>();
            cause = error.source();
        }
        // hyper reports an HTTP/1 connection that failed mid-body without a more specific cause.
        from_hyper.then_some(Self::Disconnected)
    }

    fn from_io(error: &io::Error) -> Self {
        match error.kind() {
            // hyper's HTTP/1 decoder reports invalid chunked encoding and similar framing
            // errors with these kinds; every other kind means the connection itself failed.
            io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => Self::Malformed,
            _ => Self::Disconnected,
        }
    }
}

/// Starts each request's [`Context`] with a [`ClientConnection`] and wraps the body hyper
/// received, so the router can tell when the client leaves or sends a malformed body.
///
/// It must sit outside decompression and every other layer that touches the body, so it sees the
/// client's body exactly as hyper delivers it.
#[derive(Clone, Default)]
pub(crate) struct ClientConnectionLayer;

impl<S> Layer<S> for ClientConnectionLayer {
    type Service = ClientConnectionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ClientConnectionService { inner }
    }
}

#[derive(Clone)]
pub(crate) struct ClientConnectionService<S> {
    inner: S,
}

impl<S> Service<http::Request<axum::body::Body>> for ClientConnectionService<S>
where
    S: Service<http::Request<axum::body::Body>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<axum::body::Body>) -> Self::Future {
        let context = Context::new();
        let connection = ClientConnection::default();
        context
            .extensions()
            .with_lock(|lock| lock.insert(connection.clone()));
        let (mut parts, body) = request.into_parts();
        parts.extensions.insert(context);
        let body = axum::body::Body::new(ClientRequestBody::new(body, connection));
        self.inner.call(http::Request::from_parts(parts, body))
    }
}

pin_project! {
    /// The request body as hyper receives it from the client.
    ///
    /// When reading fails, it records why on the request's [`ClientConnection`] before it passes
    /// the error on, so whatever reads the body later needs no special handling.
    struct ClientRequestBody<B> {
        #[pin]
        inner: B,
        connection: ClientConnection,
    }
}

impl<B> ClientRequestBody<B> {
    fn new(inner: B, connection: ClientConnection) -> Self {
        Self { inner, connection }
    }
}

impl<B> http_body::Body for ClientRequestBody<B>
where
    B: http_body::Body,
    B::Error: StdError + 'static,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let frame = ready!(this.inner.poll_frame(cx));
        if let Some(Err(error)) = &frame {
            this.connection.record_body_error(error);
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Replaces the router pipeline's outcome when the client connection decides the status: 499 if
/// the client left before it was answered, or 400 if its request body was malformed.
///
/// Router telemetry sits directly outside this, so its spans and instruments record that status.
pub(crate) async fn apply_client_connection_status(
    context: Context,
    response: impl Future<Output = Result<router::Response, BoxError>>,
) -> Result<router::Response, BoxError> {
    let response = response.await;
    let state = context.extensions().with_lock(|lock| {
        lock.get::<ClientConnection>()
            .map_or(State::Connected, ClientConnection::state)
    });
    match state {
        State::Connected => response,
        State::MalformedBody => router::Response::error_builder()
            .status_code(StatusCode::BAD_REQUEST)
            .error(
                graphql::Error::builder()
                    .message("the request body could not be read")
                    .extension_code("INVALID_GRAPHQL_REQUEST")
                    .build(),
            )
            .context(context)
            .build(),
        State::Closed => match response {
            Ok(mut response) => {
                *response.response.status_mut() = client_closed_request();
                Ok(response)
            }
            Err(_) => Ok(client_closed_response(context)),
        },
    }
}

/// The response recorded for a client that left before it was answered. Nobody will read it, so
/// it carries no body.
pub(crate) fn client_closed_response(context: Context) -> router::Response {
    router::Response {
        response: http::Response::builder()
            .status(client_closed_request())
            .body(router::body::empty())
            .expect("a status and an empty body make a valid response"),
        context,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(error: impl StdError + 'static) -> Option<BodyError> {
        BodyError::classify(&error)
    }

    fn context_with(connection: &ClientConnection) -> Context {
        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert(connection.clone()));
        context
    }

    #[test]
    fn a_connection_that_ends_or_fails_is_a_disconnect() {
        for kind in [
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
        ] {
            assert_eq!(
                classify(io::Error::from(kind)),
                Some(BodyError::Disconnected),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn invalid_framing_is_a_malformed_body() {
        for kind in [io::ErrorKind::InvalidData, io::ErrorKind::InvalidInput] {
            assert_eq!(
                classify(io::Error::from(kind)),
                Some(BodyError::Malformed),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_wrapped_io_error_is_classified_by_its_kind() {
        let wrapped = axum::Error::new(io::Error::from(io::ErrorKind::ConnectionReset));
        assert_eq!(classify(wrapped), Some(BodyError::Disconnected));
    }

    #[test]
    fn an_error_from_outside_hyper_is_not_classified() {
        let error: BoxError = "something else".into();
        assert_eq!(BodyError::classify(&*error), None);
    }

    #[test]
    fn a_closed_connection_stays_closed() {
        let connection = ClientConnection::default();
        connection.close();
        connection.reject_body();
        assert_eq!(connection.state(), State::Closed);
    }

    #[test]
    fn closing_overrides_a_malformed_body() {
        let connection = ClientConnection::default();
        connection.reject_body();
        assert_eq!(connection.state(), State::MalformedBody);
        connection.close();
        assert_eq!(connection.state(), State::Closed);
    }

    #[tokio::test]
    async fn an_error_from_a_closed_connection_becomes_a_499() {
        let connection = ClientConnection::default();
        connection.close();
        let context = context_with(&connection);

        let response =
            apply_client_connection_status(context, async { Err("body read failed".into()) })
                .await
                .expect("a closed connection answers with a response");

        assert_eq!(response.response.status(), client_closed_request());
    }

    #[tokio::test]
    async fn a_success_after_the_client_left_becomes_a_499() {
        let connection = ClientConnection::default();
        connection.close();
        let context = context_with(&connection);
        let ok = router::Response::fake_builder()
            .context(context.clone())
            .build()
            .unwrap();

        let response = apply_client_connection_status(context, async { Ok(ok) })
            .await
            .unwrap();

        assert_eq!(response.response.status(), client_closed_request());
    }

    #[tokio::test]
    async fn a_malformed_body_becomes_a_400() {
        let connection = ClientConnection::default();
        connection.reject_body();
        let context = context_with(&connection);

        let response =
            apply_client_connection_status(context, async { Err("body read failed".into()) })
                .await
                .unwrap();

        assert_eq!(response.response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_error_while_the_client_is_connected_is_unchanged() {
        let context = context_with(&ClientConnection::default());

        let error = apply_client_connection_status(context, async {
            Err(io::Error::from(io::ErrorKind::ConnectionReset).into())
        })
        .await
        .expect_err("an outbound error keeps its error");

        assert!(error.is::<io::Error>());
    }

    #[tokio::test]
    async fn a_context_without_a_connection_counts_as_connected() {
        let error = apply_client_connection_status(Context::new(), async {
            Err("subgraph request failed".into())
        })
        .await
        .expect_err("the error is unchanged");

        assert_eq!(error.to_string(), "subgraph request failed");
    }
}
