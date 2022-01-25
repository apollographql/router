use bytes::{BufMut, Bytes, BytesMut};
use futures::{future::BoxFuture, Future};
use serde::Serialize;
use serde_json::{ser::CharEscape, Serializer};
use serde_json_bytes::{ByteString, Value};
use std::{
    cmp::min,
    io::{self, Write},
    pin::Pin,
    task::Poll,
};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::io::ReaderStream;

pub enum Error {
    IO(std::io::Error),
    Serde(serde_json::Error),
}

pub struct BytesWriter {
    sender: mpsc::Sender<Bytes>,
    buffer: Option<BytesMut>,
    buffer_capacity: usize,
}

impl BytesWriter {
    pub fn new(buffer_capacity: usize, channel_capacity: usize) -> (Self, mpsc::Receiver<Bytes>) {
        let (sender, receiver) = mpsc::channel(channel_capacity);

        (
            BytesWriter {
                sender,
                buffer: None,
                buffer_capacity,
            },
            receiver,
        )
    }

    pub fn serialize<T: Serialize>(self, data: T) -> Result<(), Error> {
        let mut ser = Serializer::new(self);
        data.serialize(&mut ser).map_err(Error::Serde)?;
        let mut writer = ser.into_inner();
        std::io::Write::flush(&mut writer).map_err(Error::IO)
    }
}

impl Write for BytesWriter {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        //println!("will write '{}'", from_utf8(buf).unwrap());

        let mut buffer = match self.buffer.take() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(self.buffer_capacity),
        };
        //println!("buf remaining: {}", buffer.capacity() - buffer.len());

        let mut size = 0;
        loop {
            let to_write = min(buf.len(), buffer.capacity() - buffer.len());
            let mut writer = buffer.writer();

            let sz = writer.write(&buf[..to_write]).unwrap();
            size += sz;

            buffer = writer.into_inner();

            if buffer.capacity() - buffer.len() > 0 {
                self.buffer = Some(buffer);
                //println!("wrote {} bytes", size);
                return Ok(size);
            } else {
                //println!("=======> will send {}", from_utf8(&buffer).unwrap());
                self.sender.blocking_send(buffer.freeze()).unwrap();

                if sz == buf.len() {
                    //println!("wrote {} bytes", size);
                    return Ok(size);
                } else {
                    buf = &buf[sz..];
                    buffer = BytesMut::with_capacity(self.buffer_capacity);
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(buffer) = self.buffer.take() {
            //println!("=======> will flush {}", from_utf8(&buffer).unwrap());
            self.sender.blocking_send(buffer.freeze()).unwrap();
        }
        Ok(())
    }
}

pub fn make_body(value: Value) -> hyper::Body {
    let (mut write, read) = tokio::io::duplex(2048);
    let read_stream = ReaderStream::new(read);
    tokio::task::spawn(async move { async_serialize(&value, &mut write).await });

    hyper::Body::wrap_stream(read_stream)
}

pub fn async_serialize<'a, W>(value: &'a Value, w: &'a mut W) -> BoxFuture<'a, io::Result<usize>>
where
    W: AsyncWrite + std::marker::Unpin + Send,
{
    Box::pin(async move {
        match value {
            Value::Null => w.write(b"null").await,
            Value::Bool(b) => {
                if *b {
                    w.write(b"true").await
                } else {
                    w.write(b"false").await
                }
            }
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    let mut buffer = itoa::Buffer::new();
                    let s = buffer.format(i);

                    w.write(s.as_bytes()).await
                } else if let Some(u) = n.as_u64() {
                    let mut buffer = itoa::Buffer::new();
                    let s = buffer.format(u);

                    w.write(s.as_bytes()).await
                } else if let Some(f) = n.as_f64() {
                    let mut buffer = ryu::Buffer::new();
                    let s = buffer.format_finite(f);

                    w.write(s.as_bytes()).await
                } else {
                    Ok(0)
                }
            }
            Value::String(s) => async_serialize_bytestring(s, w).await,
            Value::Array(a) => {
                let mut sz = w.write(b"[").await?;

                let mut it = a.iter();

                if let Some(v) = it.next() {
                    sz += async_serialize(v, w).await?;

                    for v in it {
                        w.write(b",").await?;
                        sz += async_serialize(v, w).await?;
                    }
                }

                sz += w.write(b"]").await?;

                Ok(sz)
            }
            Value::Object(o) => {
                let mut sz = w.write(b"{").await?;

                let mut it = o.iter();

                if let Some((key, v)) = it.next() {
                    sz += async_serialize_bytestring(key, w).await?;
                    w.write(b":").await?;
                    sz += async_serialize(v, w).await?;

                    for (key, v) in it {
                        w.write(b",").await?;

                        sz += async_serialize_bytestring(key, w).await?;
                        w.write(b":").await?;
                        sz += async_serialize(v, w).await?;
                    }
                }

                sz += w.write(b"}").await?;

                Ok(sz)
            }
        }
    })
}

async fn async_serialize_bytestring<W>(s: &ByteString, w: &mut W) -> io::Result<usize>
where
    W: AsyncWrite + std::marker::Unpin,
{
    let mut sz = w.write(b"\"").await?;

    let bytes = s.as_str().as_bytes();
    let mut start = 0;

    for (i, &byte) in bytes.iter().enumerate() {
        let escape = ESCAPE[byte as usize];
        if escape == 0 {
            continue;
        }

        if start < i {
            sz += w.write(&bytes[start..i]).await?;
        }

        let char_escape = from_escape_table(escape, byte);
        sz += write_char_escape(w, char_escape).await?;

        start = i + 1;
    }

    if start != bytes.len() {
        sz += w.write(&bytes[start..]).await?;
    }

    sz += w.write(b"\"").await?;

    Ok(sz)
}

async fn write_char_escape<W>(writer: &mut W, char_escape: CharEscape) -> io::Result<usize>
where
    W: AsyncWrite + std::marker::Unpin,
{
    use CharEscape::*;

    let s = match char_escape {
        Quote => b"\\\"",
        ReverseSolidus => b"\\\\",
        Solidus => b"\\/",
        Backspace => b"\\b",
        FormFeed => b"\\f",
        LineFeed => b"\\n",
        CarriageReturn => b"\\r",
        Tab => b"\\t",
        AsciiControl(byte) => {
            static HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";
            let bytes = &[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX_DIGITS[(byte >> 4) as usize],
                HEX_DIGITS[(byte & 0xF) as usize],
            ];
            return writer.write(bytes).await;
        }
    };

    writer.write(s).await
}

fn from_escape_table(escape: u8, byte: u8) -> CharEscape {
    match escape {
        self::BB => CharEscape::Backspace,
        self::TT => CharEscape::Tab,
        self::NN => CharEscape::LineFeed,
        self::FF => CharEscape::FormFeed,
        self::RR => CharEscape::CarriageReturn,
        self::QU => CharEscape::Quote,
        self::BS => CharEscape::ReverseSolidus,
        self::UU => CharEscape::AsciiControl(byte),
        _ => unreachable!(),
    }
}

const BB: u8 = b'b'; // \x08
const TT: u8 = b't'; // \x09
const NN: u8 = b'n'; // \x0A
const FF: u8 = b'f'; // \x0C
const RR: u8 = b'r'; // \x0D
const QU: u8 = b'"'; // \x22
const BS: u8 = b'\\'; // \x5C
const UU: u8 = b'u'; // \x00...\x1F except the ones above
const __: u8 = 0;

// Lookup table of escape sequences. A value of b'x' at index i means that byte
// i is escaped as "\x" in JSON. A value of 0 means that byte i is not escaped.
static ESCAPE: [u8; 256] = [
    //   1   2   3   4   5   6   7   8   9   A   B   C   D   E   F
    UU, UU, UU, UU, UU, UU, UU, UU, BB, TT, NN, UU, FF, RR, UU, UU, // 0
    UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, // 1
    __, __, QU, __, __, __, __, __, __, __, __, __, __, __, __, __, // 2
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 3
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 4
    __, __, __, __, __, __, __, __, __, __, __, __, BS, __, __, __, // 5
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 6
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 7
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 8
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 9
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // A
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // B
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // C
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // D
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // E
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // F
];
