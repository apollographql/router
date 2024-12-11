use axum::Error as AxumError;
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use http_body::Frame;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use http_body_util::StreamBody;
use hyper::body::Body as HttpBody;

pub type RouterBody = UnsyncBoxBody<Bytes, AxumError>;

pub(crate) async fn get_body_bytes<B: HttpBody>(body: B) -> Result<Bytes, B::Error> {
    Ok(body.collect().await?.to_bytes())
}

// We create some utility functions to make Empty and Full bodies
// and convert types

/// Create an empty RouterBody
pub(crate) fn empty() -> UnsyncBoxBody<Bytes, AxumError> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// Create a Full RouterBody using the supplied chunk
pub(crate) fn full<T: Into<Bytes>>(chunk: T) -> UnsyncBoxBody<Bytes, AxumError> {
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

/// Utility function for converting UnsyncBoxBody to Full bodies.
///
/// Currently only used in snapshot testing
#[cfg(test)]
pub(crate) async fn unsync_to_full(
    input: http::Request<RouterBody>,
) -> Result<http::Request<Full<Bytes>>, AxumError> {
    let (parts, body) = input.into_parts();

    let body_bytes = get_body_bytes(body).await?;
    let new_request = http::Request::from_parts(parts, Full::new(body_bytes));
    Ok(new_request)
}
