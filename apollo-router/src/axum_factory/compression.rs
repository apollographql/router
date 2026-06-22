use async_compression::Level;
use async_compression::tokio::write::BrotliEncoder;
use async_compression::tokio::write::DeflateEncoder;
use async_compression::tokio::write::GzipEncoder;
use async_compression::tokio::write::ZstdEncoder;
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tower::BoxError;

use crate::services::router::body::RouterBody;

/// Abstracts the `get_mut()`/`into_inner()` inherent methods that each
/// `async-compression` encoder exposes on its `Vec<u8>` inner writer, allowing
/// `encode_stream` to be written once generically.
trait EncoderExt: tokio::io::AsyncWrite + Unpin + Send {
    fn take_output(&mut self) -> Vec<u8>;
    fn finish(self) -> Vec<u8>;
}

macro_rules! impl_encoder_ext {
    ($ty:ident) => {
        impl EncoderExt for $ty<Vec<u8>> {
            fn take_output(&mut self) -> Vec<u8> {
                std::mem::take(self.get_mut())
            }
            fn finish(self) -> Vec<u8> {
                self.into_inner()
            }
        }
    };
}
impl_encoder_ext!(GzipEncoder);
impl_encoder_ext!(DeflateEncoder);
impl_encoder_ext!(BrotliEncoder);
impl_encoder_ext!(ZstdEncoder);

async fn encode_chunk<E: EncoderExt>(encoder: &mut E, data: &[u8]) -> Result<Bytes, BoxError> {
    encoder.write_all(data).await?;
    encoder.flush().await?;
    Ok(Bytes::from(encoder.take_output()))
}

/// Returns a lazy `Stream` that compresses `stream` one chunk at a time.
///
/// Each output chunk is produced only when the consumer polls for it, which
/// preserves backpressure: the next input chunk is not fetched until the
/// previous compressed chunk has been consumed. This is critical for `@defer`
/// streaming — it ensures the first response part reaches the client before the
/// deferred subgraph call that produces the second part is even issued.
fn encode_stream<E, S, SE>(
    encoder: E,
    stream: S,
) -> impl Stream<Item = Result<Bytes, BoxError>> + Send + 'static
where
    E: EncoderExt + Send + 'static,
    S: Stream<Item = Result<Bytes, SE>> + Unpin + Send + 'static,
    SE: Into<BoxError> + Send + 'static,
{
    enum State<E, S> {
        Processing(E, S),
        Done,
    }

    futures::stream::unfold(State::Processing(encoder, stream), |state| async move {
        match state {
            State::Done => None,
            State::Processing(mut encoder, mut stream) => match stream.next().await {
                Some(Ok(data)) => {
                    let result = encode_chunk(&mut encoder, &data).await;
                    let next = if result.is_err() {
                        State::Done
                    } else {
                        State::Processing(encoder, stream)
                    };
                    Some((result, next))
                }
                Some(Err(e)) => Some((Err(e.into()), State::Done)),
                None => {
                    if let Err(e) = encoder.shutdown().await {
                        // Don't yield `remaining` after a shutdown failure: the encoder
                        // didn't write a valid finalizer, so any buffered bytes are
                        // incomplete and would corrupt the decompressor.
                        Some((Err(e.into()), State::Done))
                    } else {
                        let remaining = Bytes::from(encoder.finish());
                        if remaining.is_empty() {
                            None
                        } else {
                            Some((Ok(remaining), State::Done))
                        }
                    }
                }
            },
        }
    })
}

pub(crate) enum Compressor {
    Deflate(DeflateEncoder<Vec<u8>>),
    Gzip(GzipEncoder<Vec<u8>>),
    Brotli(Box<BrotliEncoder<Vec<u8>>>),
    Zstd(ZstdEncoder<Vec<u8>>),
}

impl Compressor {
    pub(crate) fn new<'a, It>(it: It) -> Option<Self>
    where
        It: Iterator<Item = &'a str>,
        It: 'a,
    {
        for s in it {
            match s {
                "gzip" => {
                    return Some(Compressor::Gzip(GzipEncoder::with_quality(
                        Vec::new(),
                        Level::Fastest,
                    )));
                }
                "deflate" => {
                    return Some(Compressor::Deflate(DeflateEncoder::with_quality(
                        Vec::new(),
                        Level::Fastest,
                    )));
                }
                "br" => {
                    return Some(Compressor::Brotli(Box::new(BrotliEncoder::with_quality(
                        Vec::new(),
                        Level::Precise(4), // https://github.com/dropbox/rust-brotli/issues/93
                    ))));
                }
                "zstd" => {
                    return Some(Compressor::Zstd(ZstdEncoder::with_quality(
                        Vec::new(),
                        Level::Fastest, // level 1; async-compression avoids negatives that expand output
                    )));
                }
                _ => {}
            }
        }
        None
    }

    pub(crate) fn content_encoding(&self) -> &'static str {
        match self {
            Compressor::Deflate(_) => "deflate",
            Compressor::Gzip(_) => "gzip",
            Compressor::Brotli(_) => "br",
            Compressor::Zstd(_) => "zstd",
        }
    }

    pub(crate) fn process(self, body: RouterBody) -> impl Stream<Item = Result<Bytes, BoxError>> {
        let stream = http_body_util::BodyDataStream::new(body);
        match self {
            Compressor::Gzip(encoder) => encode_stream(encoder, stream).fuse().boxed(),
            Compressor::Deflate(encoder) => encode_stream(encoder, stream).fuse().boxed(),
            Compressor::Brotli(encoder) => encode_stream(*encoder, stream).fuse().boxed(),
            Compressor::Zstd(encoder) => encode_stream(encoder, stream).fuse().boxed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use async_compression::tokio::write::BrotliDecoder;
    use async_compression::tokio::write::DeflateDecoder;
    use async_compression::tokio::write::GzipDecoder;
    use async_compression::tokio::write::ZstdDecoder;
    use futures::StreamExt as _;
    use futures::stream;
    use rand::RngExt as _;
    use rstest::rstest;
    use tokio::io::AsyncWrite;
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::services::router;
    use crate::services::router::body::{self};

    // `get_ref()` and `get_mut()` on the async-compression decoders are inherent
    // methods, not part of any trait. This thin trait lets us write a single
    // generic helper for the flush tests rather than repeating the body four times.
    trait DecoderTestExt: AsyncWrite + Unpin {
        fn decoded(&self) -> &[u8];
        fn decoded_mut(&mut self) -> &mut Vec<u8>;
    }

    macro_rules! impl_decoder_test_ext {
        ($ty:ident) => {
            impl DecoderTestExt for $ty<Vec<u8>> {
                fn decoded(&self) -> &[u8] {
                    self.get_ref()
                }
                fn decoded_mut(&mut self) -> &mut Vec<u8> {
                    self.get_mut()
                }
            }
        };
    }
    impl_decoder_test_ext!(GzipDecoder);
    impl_decoder_test_ext!(DeflateDecoder);
    impl_decoder_test_ext!(BrotliDecoder);
    impl_decoder_test_ext!(ZstdDecoder);

    /// Feeds `stream` to `decoder` one chunk at a time, asserting after each chunk that the
    /// decoded output so far matches the expected text. A failure here means the compressor is
    /// buffering across chunk boundaries instead of flushing a sync point after each one.
    async fn assert_per_chunk_flush(
        mut stream: impl futures::Stream<Item = Result<Bytes, BoxError>> + Unpin,
        mut decoder: Box<dyn DecoderTestExt>,
        primary: &str,
        deferred: &str,
    ) {
        let first = stream
            .next()
            .await
            .expect("stream ended before first chunk")
            .expect("first chunk error");
        decoder.write_all(&first).await.unwrap();
        decoder.flush().await.unwrap();
        decoder.decoded_mut().flush().await.unwrap();
        assert_eq!(
            std::str::from_utf8(decoder.decoded()).expect("decoded output is not valid UTF-8"),
            primary
        );

        let second = stream
            .next()
            .await
            .expect("stream ended before second chunk")
            .expect("second chunk error");
        decoder.write_all(&second).await.unwrap();
        decoder.flush().await.unwrap();
        decoder.decoded_mut().flush().await.unwrap();

        let expected = format!("{primary}{deferred}");
        assert_eq!(
            std::str::from_utf8(decoder.decoded()).expect("decoded output is not valid UTF-8"),
            expected
        );
    }

    /// Verifies that a full body compresses and decompresses correctly end-to-end for each
    /// encoding, and that the output stream is properly closed after all data is sent.
    #[rstest]
    #[case::gzip("gzip")]
    #[case::deflate("deflate")]
    #[case::brotli("br")]
    #[case::zstd("zstd")]
    #[tokio::test]
    async fn finish(#[case] encoding: &str) {
        let compressor = Compressor::new([encoding].into_iter()).unwrap();

        let mut rng = rand::rng();
        let body: RouterBody = body::from_bytes(
            std::iter::repeat(())
                .map(|_| rng.random_range(0u8..3))
                .take(5000)
                .collect::<Vec<_>>(),
        );

        let mut stream = compressor.process(body);
        let mut decoder: Box<dyn DecoderTestExt> = match encoding {
            "gzip" => Box::new(GzipDecoder::new(Vec::new())),
            "deflate" => Box::new(DeflateDecoder::new(Vec::new())),
            "br" => Box::new(BrotliDecoder::new(Vec::new())),
            "zstd" => Box::new(ZstdDecoder::new(Vec::new())),
            _ => unreachable!(),
        };

        while let Some(buf) = stream.next().await {
            decoder.write_all(&buf.unwrap()).await.unwrap();
        }

        decoder.shutdown().await.unwrap();
        assert_eq!(decoder.decoded().len(), 5000);
        assert!(stream.next().await.is_none());
    }

    /// Verifies that an error from the input body stream is forwarded to the output stream
    /// and that the output stream is closed immediately after.
    #[tokio::test]
    async fn stream_error_is_propagated() {
        let compressor = Compressor::new(["gzip"].into_iter()).unwrap();
        let body: RouterBody = router::body::from_result_stream(stream::iter(vec![
            Ok::<_, BoxError>(Bytes::from("hello")),
            Err(BoxError::from("input error")),
        ]));

        let mut stream = compressor.process(body);
        assert!(stream.next().await.unwrap().is_ok());
        assert!(stream.next().await.unwrap().is_err());
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn small_input() {
        let compressor = Compressor::new(["gzip"].into_iter()).unwrap();

        let body: RouterBody = body::from_bytes(vec![0u8, 1, 2, 3]);

        let mut stream = compressor.process(body);
        let mut decoder = GzipDecoder::new(Vec::new());

        while let Some(buf) = stream.next().await {
            let b = buf.unwrap();
            decoder.write_all(&b).await.unwrap();
        }

        decoder.shutdown().await.unwrap();
        let response = decoder.into_inner();
        assert_eq!(response, [0u8, 1, 2, 3]);

        assert!(stream.next().await.is_none());
    }

    #[rstest]
    #[case::unknown(&["unknown"])]
    #[case::identity(&["identity"])]
    #[case::empty(&[])]
    fn new_returns_none(#[case] encodings: &[&str]) {
        assert!(Compressor::new(encodings.iter().copied()).is_none());
    }

    #[rstest]
    #[case::zstd_beats_gzip(&["zstd", "gzip"], "zstd")]
    #[case::skips_unknown(&["unknown", "br"], "br")]
    fn new_returns_first_supported_encoding(#[case] encodings: &[&str], #[case] expected: &str) {
        let c = Compressor::new(encodings.iter().copied()).unwrap();
        assert_eq!(c.content_encoding(), expected);
    }

    /// Verifies that `encode_chunk` compresses bytes and flushes a sync point that allows
    /// partial decompression without closing the stream.
    #[tokio::test]
    async fn encode_chunk_roundtrip() {
        let mut encoder = GzipEncoder::with_quality(Vec::new(), Level::Fastest);
        let input = b"hello, world";
        let compressed = encode_chunk(&mut encoder, input).await.unwrap();

        let mut decoder = GzipDecoder::new(Vec::new());
        decoder.write_all(&compressed).await.unwrap();
        decoder.flush().await.unwrap();
        decoder.get_mut().flush().await.unwrap();
        assert_eq!(decoder.get_ref(), input);
    }

    #[tokio::test]
    async fn gzip_header_writing() {
        let compressor = Compressor::new(["gzip"].into_iter()).unwrap();
        let body: RouterBody =
            body::from_bytes(r#"{"data":{"me":{"id":"1","name":"Ada Lovelace"}}}"#);

        let mut stream = compressor.process(body);
        let _ = stream.next().await.unwrap().unwrap();
    }

    /// Verifies that each input chunk produces an independently decompressable output chunk.
    /// This is the critical property for `@defer` streaming: the first part of the response
    /// must reach the client before the second part is compressed.
    #[rstest]
    #[case::gzip("gzip")]
    #[case::deflate("deflate")]
    #[case::brotli("br")]
    #[case::zstd("zstd")]
    #[tokio::test]
    async fn flush(#[case] encoding: &str) {
        const PRIMARY_RESPONSE: &str = r#"
--graphql
content-type: application/json

{"data":{"allProducts":[{"sku":"federation","id":"apollo-federation"},{"sku":"studio","id":"apollo-studio"},{"sku":"client","id":"apollo-client"}]},"hasNext":true}
--graphql
"#;

        const DEFERRED_RESPONSE: &str = r#"content-type: application/json

{"hasNext":false,"incremental":[{"data":{"dimensions":{"size":"1"},"variation":{"id":"OSS","name":"platform"}},"path":["allProducts",0]},{"data":{"dimensions":{"size":"1"},"variation":{"id":"platform","name":"platform-name"}},"path":["allProducts",1]},{"data":{"dimensions":{"size":"1"},"variation":{"id":"OSS","name":"client"}},"path":["allProducts",2]}]}
--graphql--
"#;

        let compressor = Compressor::new([encoding].into_iter()).unwrap();
        let body: RouterBody = router::body::from_result_stream(stream::iter(vec![
            Ok::<_, BoxError>(Bytes::from(PRIMARY_RESPONSE)),
            Ok(Bytes::from(DEFERRED_RESPONSE)),
        ]));
        let stream = compressor.process(body);
        let decoder: Box<dyn DecoderTestExt> = match encoding {
            "gzip" => Box::new(GzipDecoder::new(Vec::new())),
            "deflate" => Box::new(DeflateDecoder::new(Vec::new())),
            "br" => Box::new(BrotliDecoder::new(Vec::new())),
            "zstd" => Box::new(ZstdDecoder::new(Vec::new())),
            _ => unreachable!(),
        };
        assert_per_chunk_flush(stream, decoder, PRIMARY_RESPONSE, DEFERRED_RESPONSE).await
    }
}
