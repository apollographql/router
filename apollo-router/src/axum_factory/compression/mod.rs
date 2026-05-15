use brotli::enc::BrotliEncoderParams;
use bytes::Bytes;
use bytes::BytesMut;
use flate2::Compression;
use futures::Stream;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use tracing::Instrument;

use self::codec::BrotliEncoder;
use self::codec::DeflateEncoder;
use self::codec::Encode;
use self::codec::GzipEncoder;
use self::codec::ZstdEncoder;
use self::util::PartialBuffer;
use crate::services::router::body::RouterBody;

pub(crate) mod codec;
pub(crate) mod unshared;
pub(crate) mod util;

const GZIP_HEADER_LEN: usize = 10;

pub(crate) enum Compressor {
    Deflate(DeflateEncoder),
    Gzip(GzipEncoder),
    Brotli(Box<BrotliEncoder>),
    Zstd(ZstdEncoder),
}

impl Compressor {
    pub(crate) fn new<'a, It>(it: It) -> Option<Self>
    where
        It: Iterator<Item = &'a str>,
        It: 'a,
    {
        for s in it {
            match s {
                "gzip" => return Some(Compressor::Gzip(GzipEncoder::new(Compression::fast()))),
                "deflate" => {
                    return Some(Compressor::Deflate(
                        DeflateEncoder::new(Compression::fast()),
                    ));
                }
                "br" => {
                    return Some(Compressor::Brotli(Box::new(BrotliEncoder::new(
                        BrotliEncoderParams {
                            // '4' is a reasonable setting for 'fast'
                            // https://github.com/dropbox/rust-brotli/issues/93
                            quality: 4,
                            ..BrotliEncoderParams::default()
                        },
                    ))));
                }
                "zstd" => {
                    return Some(Compressor::Zstd(ZstdEncoder::new(zstd_safe::min_c_level())));
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

    pub(crate) fn process(
        mut self,
        body: RouterBody,
    ) -> impl Stream<Item = Result<Bytes, BoxError>>
where {
        let (tx, rx) = mpsc::channel(10);

        let mut stream = http_body_util::BodyDataStream::new(body);
        tokio::task::spawn(
            async move {
                while let Some(data) = stream.next().await {
                    match data {
                        Err(e) => {
                            if (tx.send(Err(e.into())).await).is_err() {
                                return;
                            }
                        }
                        Ok(data) => {
                            // the buffer needs at least 10 bytes for a gzip header if we use gzip, then more
                            // room to store the data itself
                            let mut buf = BytesMut::zeroed(GZIP_HEADER_LEN + data.len());

                            let mut partial_input = PartialBuffer::new(&*data);
                            let mut partial_output = PartialBuffer::new(&mut buf);
                            loop {
                                if let Err(e) = self.encode(&mut partial_input, &mut partial_output)
                                {
                                    let _ = tx.send(Err(e.into())).await;
                                    return;
                                }

                                if !partial_input.unwritten().is_empty() {
                                    // there was not enough space in the output buffer to compress everything,
                                    // so we resize and add more data
                                    if partial_output.unwritten().is_empty() {
                                        partial_output.extend(partial_input.unwritten().len() / 10);
                                    }
                                } else {
                                    loop {
                                        match self.flush(&mut partial_output) {
                                            Err(e) => {
                                                let _ = tx.send(Err(e.into())).await;
                                                return;
                                            }
                                            Ok(flushed) => {
                                                if flushed {
                                                    break;
                                                }
                                                if partial_output.unwritten().is_empty() {
                                                    partial_output
                                                        .extend(partial_output.written().len());
                                                }
                                            }
                                        }
                                    }

                                    let len = partial_output.written().len();
                                    let _ = partial_output.into_inner();
                                    buf.resize(len, 0);

                                    if (tx.send(Ok(buf.freeze())).await).is_err() {
                                        return;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }

                loop {
                    let buf = BytesMut::zeroed(1024);
                    let mut partial_output = PartialBuffer::new(buf);

                    match self.finish(&mut partial_output) {
                        Err(e) => {
                            let _ = tx.send(Err(e.into())).await;
                            break;
                        }
                        Ok(is_flushed) => {
                            let len = partial_output.written().len();

                            let mut buf = partial_output.into_inner();
                            buf.resize(len, 0);
                            if (tx.send(Ok(buf.freeze())).await).is_err() {
                                return;
                            }
                            if is_flushed {
                                break;
                            }
                        }
                    }
                }
            }
            .instrument(tracing::debug_span!("body_compression")),
        );
        ReceiverStream::new(rx)
    }
}

impl Encode for Compressor {
    fn encode(
        &mut self,
        input: &mut PartialBuffer<impl AsRef<[u8]>>,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> std::io::Result<()> {
        match self {
            Compressor::Deflate(e) => e.encode(input, output),
            Compressor::Gzip(e) => e.encode(input, output),
            Compressor::Brotli(e) => e.encode(input, output),
            Compressor::Zstd(e) => e.encode(input, output),
        }
    }

    fn flush(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> std::io::Result<bool> {
        match self {
            Compressor::Deflate(e) => e.flush(output),
            Compressor::Gzip(e) => e.flush(output),
            Compressor::Brotli(e) => e.flush(output),
            Compressor::Zstd(e) => e.flush(output),
        }
    }

    fn finish(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> std::io::Result<bool> {
        match self {
            Compressor::Deflate(e) => e.finish(output),
            Compressor::Gzip(e) => e.finish(output),
            Compressor::Brotli(e) => e.finish(output),
            Compressor::Zstd(e) => e.finish(output),
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

    #[tokio::test]
    async fn finish() {
        let compressor = Compressor::new(["gzip"].into_iter()).unwrap();

        let mut rng = rand::rng();
        let body: RouterBody = body::from_bytes(
            std::iter::repeat(())
                .map(|_| rng.random_range(0u8..3))
                .take(5000)
                .collect::<Vec<_>>(),
        );

        let mut stream = compressor.process(body);
        let mut decoder = GzipDecoder::new(Vec::new());

        while let Some(buf) = stream.next().await {
            decoder.write_all(&buf.unwrap()).await.unwrap();
        }

        decoder.shutdown().await.unwrap();
        let response = decoder.into_inner();
        assert_eq!(response.len(), 5000);

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
