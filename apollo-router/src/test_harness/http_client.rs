use std::io;
use std::pin::Pin;
use std::task::Poll;

use async_compression::tokio::bufread::BrotliDecoder;
use axum::body::BoxBody;
use futures::stream::poll_fn;
use futures::Future;
use futures::Stream;
use futures::StreamExt;
use http::HeaderValue;
use http_body::Body;
use mediatype::MediaType;
use mediatype::ReadParams;
use mime::APPLICATION_JSON;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio_util::io::StreamReader;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;

/// Added by `response_decompression` to `http::Response::extensions`
pub(crate) struct ResponseBodyWasCompressed(pub(crate) bool);

pub(crate) enum MaybeMultipart<Part> {
    Multipart(Pin<Box<dyn Stream<Item = Part> + Send>>),
    NotMultipart(Part),
}

impl<Part> MaybeMultipart<Part> {
    pub(crate) fn expect_multipart(self) -> Pin<Box<dyn Stream<Item = Part> + Send>> {
        match self {
            MaybeMultipart::Multipart(stream) => stream,
            MaybeMultipart::NotMultipart(_) => panic!("expected a multipart response"),
        }
    }

    pub(crate) fn expect_not_multipart(self) -> Part {
        match self {
            MaybeMultipart::Multipart(_) => panic!("expected a non-multipart response"),
            MaybeMultipart::NotMultipart(part) => part,
        }
    }
}

pub(crate) fn response_decompression<InnerService, RequestBody>(
    inner: InnerService,
) -> impl Service<
    http::Request<RequestBody>,
    Response = http::Response<Pin<Box<dyn AsyncRead + Send>>>,
    Error = BoxError,
>
where
    InnerService:
        Service<http::Request<RequestBody>, Response = http::Response<BoxBody>, Error = BoxError>,
{
    ServiceBuilder::new()
        .map_request(|mut request: http::Request<RequestBody>| {
            request
                .headers_mut()
                .insert("accept-encoding", "br".try_into().unwrap());
            request
        })
        .map_response(|response: http::Response<BoxBody>| {
            let mut response = response.map(|body| {
                // Convert from axum’s BoxBody to AsyncBufRead
                let mut body = Box::pin(body);
                let stream = poll_fn(move |ctx| body.as_mut().poll_data(ctx))
                    .map(|result| result.map_err(|e| io::Error::new(io::ErrorKind::Other, e)));
                StreamReader::new(stream)
            });
            let content_encoding = response.headers().get("content-encoding");
            if let Some(encoding) = content_encoding {
                assert_eq!(
                    encoding.as_bytes(),
                    b"br",
                    "unexpected content-encoding: {:?}",
                    String::from_utf8_lossy(encoding.as_bytes())
                );
            }
            let compressed = content_encoding.is_some();
            response
                .extensions_mut()
                .insert(ResponseBodyWasCompressed(compressed));
            if compressed {
                response.map(|body| Box::pin(BrotliDecoder::new(body)) as _)
            } else {
                response.map(|body| Box::pin(body) as _)
            }
        })
        .service(inner)
}

pub(crate) fn defer_spec_20220824_multipart<InnerService, RequestBody>(
    inner: InnerService,
) -> impl Service<
    http::Request<RequestBody>,
    Response = http::Response<MaybeMultipart<Vec<u8>>>,
    Error = BoxError,
>
where
    InnerService: Service<
        http::Request<RequestBody>,
        Response = http::Response<Pin<Box<dyn AsyncRead + Send>>>,
        Error = BoxError,
    >,
{
    ServiceBuilder::new()
        .map_request(|mut request: http::Request<RequestBody>| {
            request.headers_mut().insert(
                "accept",
                "multipart/mixed;deferSpec=20220824".try_into().unwrap(),
            );
            request
        })
        .map_future(|future| async {
            let response: http::Response<Pin<Box<dyn AsyncRead + Send>>> = future.await?;
            let (parts, mut body) = response.into_parts();
            let content_type = parts.headers.get("content-type").unwrap();
            let media_type = MediaType::parse(content_type.to_str().unwrap()).unwrap();
            let body = if media_type.ty == "multipart" {
                let defer_spec = mediatype::Name::new("deferSpec").unwrap();
                assert_eq!(media_type.subty, "mixed");
                assert_eq!(media_type.get_param(defer_spec).unwrap(), "20220824");
                let boundary = media_type.get_param(mediatype::names::BOUNDARY).unwrap();
                let boundary = format!("\r\n--{}", boundary.unquoted_str());
                MaybeMultipart::Multipart(parse_multipart(boundary, body).await)
            } else {
                let mut vec = Vec::new();
                body.read_to_end(&mut vec).await.unwrap();
                MaybeMultipart::NotMultipart(vec)
            };
            Ok(http::Response::from_parts(parts, body))
        })
        .service(inner)
}

async fn parse_multipart(
    boundary: String,
    mut body: Pin<Box<dyn AsyncRead + Send>>,
) -> Pin<Box<dyn Stream<Item = Vec<u8>> + Send>> {
    let mut buffer = Vec::new();
    while buffer.len() < boundary.len() {
        read_some_more(&mut body, &mut buffer).await;
    }
    assert_prefix(&buffer, &boundary);
    buffer.drain(..boundary.len());

    let mut future = Some(Box::pin(read_part(body, boundary, buffer)));
    futures::stream::poll_fn(move |ctx| {
        if let Some(f) = &mut future {
            match f.as_mut().poll(ctx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => {
                    future = None;
                    Poll::Ready(None)
                }
                // Juggle ownership of `boundary` and `next_buffer`
                // across multiple instances of async-fn-returned futures.
                Poll::Ready(Some((body, boundary, part, next_buffer))) => {
                    future = Some(Box::pin(read_part(body, boundary, next_buffer)));
                    Poll::Ready(Some(part))
                }
            }
        } else {
            Poll::Ready(None)
        }
    })
    .boxed()
}

/// Reads one part of `multipart/mixed`
///
/// To be called when the position of `body` is just after a multipart boundary
///
/// Returns `Some((body, boundary, part, next_buffer))`,
/// or `None` when there is no further part.
async fn read_part(
    mut body: Pin<Box<dyn AsyncRead + Send>>,
    boundary: String,
    mut buffer: Vec<u8>,
) -> Option<(Pin<Box<dyn AsyncRead + Send>>, String, Vec<u8>, Vec<u8>)> {
    const BOUNDARY_SUFFIX_LEN: usize = 2;
    while buffer.len() < BOUNDARY_SUFFIX_LEN {
        read_some_more(&mut body, &mut buffer).await;
    }
    let boundary_suffix = &buffer[..BOUNDARY_SUFFIX_LEN];
    match boundary_suffix {
        b"--" => return None, // This boundary marked the end of multipart
        b"\r\n" => {}         // Another part follows
        _ => panic!("unexpected boundary suffix"),
    };
    buffer.drain(..BOUNDARY_SUFFIX_LEN);

    loop {
        // Restarting the substring seach from the start of `part` at every iteration
        // makes this overall loop take O(n²) time.
        // This is good enough for tests with known-small responses,
        // and makes it easier to account for multipart boundaries
        // that might be split across multiple reads.
        if let Some(before_boundary) = memchr::memmem::find(&buffer, boundary.as_bytes()) {
            let part = buffer[..before_boundary].to_vec();
            let after_boundary = before_boundary + boundary.len();
            buffer.drain(..after_boundary);
            return Some((body, boundary, part, buffer));
        }
        read_some_more(&mut body, &mut buffer).await;
    }
}

// Similar to AsyncBufRead::fill_buf, but reads the stream even if the buffer is not empty.
// This allows searching for patterns more than one byte long.
async fn read_some_more(body: &mut Pin<Box<dyn AsyncRead + Send>>, buffer: &mut Vec<u8>) {
    const BUFFER_SIZE_INCREMENT: usize = 1024;
    let previous_len = buffer.len();
    buffer.resize(previous_len + BUFFER_SIZE_INCREMENT, 0);
    let read_len = body.read(&mut buffer[previous_len..]).await.unwrap();
    if read_len == 0 {
        panic!("end of response body without a multipart end boundary")
    }
    buffer.truncate(previous_len + read_len);
}

fn assert_prefix<'a>(bytes: &'a [u8], expected_prefix: &str) -> &'a [u8] {
    let (prefix, rest) = bytes.split_at(expected_prefix.len().min(bytes.len()));
    assert_eq!(
        prefix,
        expected_prefix.as_bytes(),
        "{:?} != {:?}",
        String::from_utf8_lossy(prefix),
        expected_prefix
    );
    rest
}

pub(crate) fn json<InnerService>(
    inner: InnerService,
) -> impl Service<
    http::Request<serde_json::Value>,
    Response = http::Response<MaybeMultipart<serde_json::Value>>,
    Error = BoxError,
>
where
    InnerService: Service<
        http::Request<hyper::Body>,
        Response = http::Response<MaybeMultipart<Vec<u8>>>,
        Error = BoxError,
    >,
{
    ServiceBuilder::new()
        .map_request(|mut request: http::Request<serde_json::Value>| {
            request.headers_mut().insert(
                "content-type",
                HeaderValue::from_static(APPLICATION_JSON.essence_str()),
            );
            request.map(|body| serde_json::to_vec(&body).unwrap().into())
        })
        .map_response(|response: http::Response<MaybeMultipart<Vec<u8>>>| {
            let (parts, body) = response.into_parts();
            let body = match body {
                MaybeMultipart::NotMultipart(bytes) => {
                    assert_eq!(
                        parts.headers.get("content-type").unwrap(),
                        APPLICATION_JSON.essence_str()
                    );
                    MaybeMultipart::NotMultipart(serde_json::from_slice(&bytes).unwrap())
                }
                MaybeMultipart::Multipart(stream) => MaybeMultipart::Multipart(
                    stream
                        .map(|part| {
                            let expected_headers = "content-type: application/json\r\n\r\n";
                            serde_json::from_slice(assert_prefix(&part, expected_headers)).unwrap()
                        })
                        .boxed(),
                ),
            };
            http::Response::from_parts(parts, body)
        })
        .service(inner)
}
