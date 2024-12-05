#![allow(deprecated)]
use std::fmt::Debug;

use axum::Error as AxumError;
use bytes::Bytes;
use futures::StreamExt;
use http_body::Frame;
use http_body::SizeHint;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyDataStream;
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
pub(crate) fn empty() -> UnsyncBoxBody<Bytes, AxumError> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync()
}

pub(crate) fn full<T: Into<Bytes>>(chunk: T) -> UnsyncBoxBody<Bytes, AxumError> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

pub(crate) fn from_data_stream(data_stream: BodyDataStream<RouterBody>) -> RouterBody {
    RouterBody::new(StreamBody::new(data_stream.map(|s| s.map(Frame::data))))
}

// Useful Conversion notes:
//  - If you have a body and want to convert it to BodyDataStream
//    You can call `body.into_data_stream` from BodyExt
//  - If you have a Stream and want a StreamBody, you can call
//    `StreamBody::new(stream)`.
//
//  I'll leave these functions here as examples and at some point
//  in the upgrade we can remove them.
//
//  Commenting out for now to prevent compiler warnings. Evaluate
//  their utility before we merge...

/*
pub(crate) fn into_data_stream_body(
    body: RouterBody,
) -> http_body_util::BodyDataStream<RouterBody> {
    body.into_data_stream()
}

pub(crate) fn into_stream_body<S, E>(stream: S) -> http_body_util::StreamBody<S>
where
    S: futures::Stream<Item = Result<Frame<RouterBody>, E>>,
{
    http_body_util::StreamBody::new(stream)
}

pub(crate) fn from_result_stream<S>(data_stream: S) -> RouterBody
where
    S: Stream<Item = Result<Bytes, AxumError>> + Send + 'static,
{
    RouterBody::new(StreamBody::new(
        data_stream.map(|s| s.map(|body| Frame::data(body))),
    ))
}
*/
