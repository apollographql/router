use brotli::enc::BrotliEncoderParams;
use bytes::Bytes;
use bytes::BytesMut;
use flate2::Compression;
use futures::Stream;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;

use self::codec::BrotliEncoder;
use self::codec::DeflateEncoder;
use self::codec::Encode;
use self::codec::GzipEncoder;
use self::codec::ZstdEncoder;
use self::util::PartialBuffer;

pub(crate) mod codec;
pub(crate) mod unshared;
pub(crate) mod util;

pub(crate) enum Compressor {
    Deflate(DeflateEncoder),
    Gzip(GzipEncoder),
    Brotli(Box<BrotliEncoder>),
    Zstd(ZstdEncoder),
}

impl Compressor {
    pub(crate) fn new<'a, It: 'a>(it: It) -> Option<Self>
    where
        It: Iterator<Item = &'a str>,
    {
        for s in it {
            match s {
                "gzip" => return Some(Compressor::Gzip(GzipEncoder::new(Compression::fast()))),
                "deflate" => {
                    return Some(Compressor::Deflate(
                        DeflateEncoder::new(Compression::fast()),
                    ))
                }
                // FIXME: find the "fast" brotli encoder params
                "br" => {
                    return Some(Compressor::Brotli(Box::new(BrotliEncoder::new(
                        BrotliEncoderParams::default(),
                    ))))
                }
                "zstd" => {
                    return Some(Compressor::Zstd(ZstdEncoder::new(zstd_safe::min_c_level())))
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
        mut stream: hyper::Body,
    ) -> impl Stream<Item = Result<Bytes, BoxError>>
where {
        let (tx, rx) = mpsc::channel(10);

        tokio::task::spawn(async move {
            while let Some(data) = stream.next().await {
                match data {
                    Err(e) => {
                        if (tx.send(Err(e.into())).await).is_err() {
                            return;
                        }
                    }
                    Ok(data) => {
                        // most compression algorithms have a compression ratio of more than 90% for JSON
                        let mut buf = BytesMut::zeroed(data.len() / 10);

                        let mut partial_input = PartialBuffer::new(&*data);
                        let mut partial_output = PartialBuffer::new(&mut buf);
                        loop {
                            if let Err(e) = self.encode(&mut partial_input, &mut partial_output) {
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
                                match self.flush(&mut partial_output) {
                                    Err(e) => {
                                        let _ = tx.send(Err(e.into())).await;
                                        return;
                                    }
                                    Ok(_) => {
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
        });
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
    use async_compression::tokio::write::GzipDecoder;
    use hyper::Body;
    use rand::Rng;
    use tokio::io::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn finish() {
        let compressor = Compressor::new(["gzip"].into_iter()).unwrap();

        let mut rng = rand::thread_rng();
        let body: Body = std::iter::repeat(())
            .map(|_| rng.gen_range(0u8..3))
            .take(5000)
            .collect::<Vec<_>>()
            .into();

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
    async fn gzip_header_writing() {
        let compressor = Compressor::new(["gzip"].into_iter()).unwrap();
        let body: Body = r#"{"data":{"me":{"id":"1","name":"Ada Lovelace"}}}"#.into();

        let mut stream = compressor.process(body);
        let _ = stream.next().await.unwrap().unwrap();
    }
}
