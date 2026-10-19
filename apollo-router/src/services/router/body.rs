use axum::Error as AxumError;
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use http_body::Frame;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use http_body_util::StreamBody;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::body::Body as HttpBody;

pub type RouterBody = UnsyncBoxBody<Bytes, AxumError>;

pub(crate) async fn into_bytes<B: HttpBody>(body: B) -> Result<Bytes, B::Error> {
    Ok(body.collect().await?.to_bytes())
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
