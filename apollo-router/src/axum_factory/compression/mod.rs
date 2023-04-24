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
    Brotli(BrotliEncoder),
    Zstd(ZstdEncoder),
}

//FIXME: we should call finish at the end
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
                    return Some(Compressor::Brotli(BrotliEncoder::new(
                        BrotliEncoderParams::default(),
                    )))
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
                        if let Err(_) = tx.send(Err(e.into())).await {
                            return;
                        }
                    }
                    Ok(data) => {
                        let mut buf = BytesMut::zeroed(1024);
                        let mut written = 0usize;

                        let mut partial_input = PartialBuffer::new(&*data);
                        loop {
                            let mut partial_output = PartialBuffer::new(&mut buf);
                            partial_output.advance(written);

                            match self.encode(&mut partial_input, &mut partial_output) {
                                Err(e) => panic!("{e:?}"),
                                Ok(()) => {}
                            }

                            //let read = partial_input.written().len();
                            written += partial_output.written().len();
                            //println!("encode: read from input: {read}, written = {written}");

                            if !partial_input.unwritten().is_empty() {
                                // there was not enough space in the output buffer to compress everything,
                                // so we resize and add more data
                                if partial_output.unwritten().is_empty() {
                                    let _ = partial_output.into_inner();
                                    buf.reserve(written);
                                }
                            } else {
                                // FIXME: what happens if we try to flush in a full buffer
                                match self.flush(&mut partial_output) {
                                    Err(e) => panic!("{e:?}"),
                                    Ok(_) => {
                                        let flushed = partial_output.written().len() - written;
                                        //println!("flush with buffer of size {flushed}");
                                        let _ = partial_output.into_inner();
                                        buf.resize(flushed, 0);
                                        if let Err(_) = tx.send(Ok(buf.freeze())).await {
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

            let buf = BytesMut::zeroed(64);
            let mut partial_output = PartialBuffer::new(buf);

            match self.finish(&mut partial_output) {
                Err(e) => panic!("{e:?}"),
                Ok(_) => {
                    let len = partial_output.written().len();
                    //println!("finish with buffer of size {}", len);

                    let mut buf = partial_output.into_inner();
                    buf.resize(len, 0);
                    if let Err(_) = tx.send(Ok(buf.freeze())).await {
                        return;
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
