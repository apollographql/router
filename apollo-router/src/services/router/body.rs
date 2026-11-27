use std::error::Error as StdError;
use std::fmt;
use std::io::ErrorKind;

use axum::Error as AxumError;
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use http::StatusCode;
use http_body::Frame;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use http_body_util::Limited;
use http_body_util::StreamBody;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::body::Body as HttpBody;
use tower::BoxError;

pub type RouterBody = UnsyncBoxBody<Bytes, AxumError>;

/// nginx-style 499: the client closed the connection. Not in the HTTP RFCs, but
/// already used by the router when a request is canceled after the body is read.
pub(crate) fn client_closed_request_status() -> StatusCode {
    StatusCode::from_u16(499).expect("499 is not a standard status code but common enough")
}

/// Incoming client-body read failed because the client disconnected.
///
/// Created only at inbound `RouterBody` collection sites, so a later
/// `ConnectionReset` from a backend/coprocessor HTTP call is not mistaken for
/// a client abort.
#[derive(Debug)]
pub(crate) struct ClientRequestBodyReadError {
    source: BoxError,
}

impl fmt::Display for ClientRequestBodyReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "client closed the connection while the request body was being read: {}",
            self.source
        )
    }
}

impl StdError for ClientRequestBodyReadError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// True when `err` (or a source) is a `ClientRequestBodyReadError`.
pub(crate) fn is_client_request_body_read_error(err: &(dyn StdError + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(e) = current {
        if e.downcast_ref::<ClientRequestBodyReadError>().is_some() {
            return true;
        }
        current = e.source();
    }
    false
}

/// True when `err` (or a source) is a client abort while reading the incoming body.
///
/// Covers hyper's incomplete/closed/canceled body errors and the IO kinds those
/// typically wrap. Only call this at inbound body-read sites — not on the
/// whole-service `BoxError` boundary, which also carries outbound I/O failures.
pub(crate) fn is_client_closed_connection(err: &(dyn StdError + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(e) = current {
        if let Some(hyper_err) = e.downcast_ref::<hyper::Error>()
            && (hyper_err.is_incomplete_message()
                || hyper_err.is_closed()
                || hyper_err.is_canceled())
        {
            return true;
        }
        if let Some(io_err) = e.downcast_ref::<std::io::Error>()
            && matches!(
                io_err.kind(),
                ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::NotConnected
            )
        {
            return true;
        }
        current = e.source();
    }
    false
}

/// Wrap an inbound body-read error as `ClientRequestBodyReadError` when it is a
/// client disconnect; otherwise pass it through unchanged.
pub(crate) fn map_client_body_read_error(
    err: impl StdError + Into<BoxError> + Send + Sync + 'static,
) -> BoxError {
    if is_client_closed_connection(&err) {
        Box::new(ClientRequestBodyReadError { source: err.into() })
    } else {
        err.into()
    }
}

pub(crate) async fn into_bytes<B: HttpBody>(body: B) -> Result<Bytes, B::Error> {
    Ok(body.collect().await?.to_bytes())
}

/// Collect an inbound client request body, tagging client-disconnect failures
/// so the HTTP boundary can return 499 without classifying backend I/O errors.
pub(crate) async fn into_client_request_bytes<B>(body: B) -> Result<Bytes, BoxError>
where
    B: HttpBody,
    B::Error: Into<BoxError> + StdError + Send + Sync + 'static,
{
    match into_bytes(body).await {
        Ok(bytes) => Ok(bytes),
        Err(e) => Err(map_client_body_read_error(e)),
    }
}

// We create some utility functions to make Empty and Full bodies
// and convert types

/// Create an empty RouterBody
pub(crate) fn empty() -> RouterBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// Create a Full RouterBody using the supplied chunk
pub fn from_bytes<T: Into<Bytes>>(chunk: T) -> RouterBody {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// Create a streaming RouterBody using the supplied stream
pub(crate) fn from_result_stream<S, E>(data_stream: S) -> RouterBody
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    S: StreamExt,
    E: Into<tower::BoxError>,
{
    RouterBody::new(StreamBody::new(
        data_stream.map(|s| s.map(Frame::data).map_err(AxumError::new)),
    ))
}

/// Like `into_bytes`, but rejects the body if it exceeds `limit` bytes.
/// Checks size per-frame as data arrives — does not buffer the full body before checking.
pub(crate) async fn into_bytes_limited<B>(body: B, limit: usize) -> Result<Bytes, BoxError>
where
    B: HttpBody,
    B::Error: Into<BoxError>,
{
    Ok(Limited::new(body, limit).collect().await?.to_bytes())
}

/// Get a body's contents as a utf-8 string for use in test assertions, or return an error.
pub async fn into_string<B>(input: B) -> Result<String, AxumError>
where
    B: HttpBody,
    B::Error: Into<axum::BoxError>,
{
    let bytes = input
        .collect()
        .await
        .map_err(AxumError::new)?
        .to_bytes()
        .to_vec();
    let string = String::from_utf8(bytes).map_err(AxumError::new)?;
    Ok(string)
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    #[test]
    fn client_closed_connection_detects_io_kinds() {
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::UnexpectedEof,
            ErrorKind::NotConnected,
        ] {
            let err = std::io::Error::new(kind, "client gone");
            assert!(
                is_client_closed_connection(&err),
                "expected {kind:?} to classify as client-closed"
            );
        }
    }

    #[test]
    fn client_closed_connection_ignores_unrelated_io() {
        let err = std::io::Error::other("disk full");
        assert!(!is_client_closed_connection(&err));
        let err = std::io::Error::new(ErrorKind::InvalidData, "bad frame");
        assert!(!is_client_closed_connection(&err));
    }

    #[test]
    fn client_closed_connection_walks_wrapped_axum_error() {
        let io_err = std::io::Error::new(ErrorKind::ConnectionReset, "connection reset by peer");
        let wrapped = AxumError::new(io_err);
        assert!(is_client_closed_connection(&wrapped));
    }

    #[test]
    fn client_closed_connection_detects_boxed_io_error() {
        let err: BoxError = std::io::Error::new(ErrorKind::ConnectionReset, "reset").into();
        assert!(is_client_closed_connection(err.as_ref()));
    }

    #[test]
    fn map_client_body_read_error_wraps_disconnect() {
        let err = std::io::Error::new(ErrorKind::ConnectionReset, "reset");
        let boxed = map_client_body_read_error(err);
        assert!(is_client_request_body_read_error(boxed.as_ref()));
    }

    #[test]
    fn map_client_body_read_error_does_not_wrap_unrelated() {
        let err = std::io::Error::other("disk full");
        let boxed = map_client_body_read_error(err);
        assert!(!is_client_request_body_read_error(boxed.as_ref()));
        assert!(boxed.downcast_ref::<std::io::Error>().is_some());
    }

    #[tokio::test]
    async fn into_bytes_limited_under_limit() {
        let body = from_bytes("hello");
        let result = into_bytes_limited(body, 10).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello");
    }

    #[tokio::test]
    async fn into_bytes_limited_at_limit() {
        let body = from_bytes("hello");
        let result = into_bytes_limited(body, 5).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello");
    }

    #[tokio::test]
    async fn into_bytes_limited_over_limit() {
        use http_body_util::LengthLimitError;

        let body = from_bytes("hello world");
        let result = into_bytes_limited(body, 5).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<LengthLimitError>()
                .is_some(),
            "error should be a LengthLimitError"
        );
    }
}
