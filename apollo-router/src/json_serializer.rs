use bytes::{BufMut, Bytes, BytesMut};
use serde_json::ser::CharEscape;
use serde_json_bytes::{ByteString, Value};
use std::{
    cmp::min,
    io::{self, Write},
};
use tokio::sync::mpsc::{self, error::SendError, Sender};
use tracing_futures::Instrument;

pub async fn make_body(value: Value) -> hyper::Body {
    let (mut acc, receiver) = BytesChunkWriter::new(2048, 10);

    let span = tracing::info_span!("serialize_response").or_current();
    tokio::task::spawn_blocking(move || {
        if serialize_all(value, &mut acc).is_err() {
            tracing::error!("failed serializing response");
        }
    })
    .instrument(span);
    hyper::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(receiver))
}

struct BytesChunkWriter {
    sender: Sender<Result<Bytes, io::Error>>,
    buffer: Option<BytesMut>,
    buffer_capacity: usize,
}

impl BytesChunkWriter {
    pub fn new(
        buffer_capacity: usize,
        channel_capacity: usize,
    ) -> (Self, mpsc::Receiver<Result<Bytes, io::Error>>) {
        let (sender, receiver) = mpsc::channel(channel_capacity);

        (
            BytesChunkWriter {
                sender,
                buffer: None,
                buffer_capacity,
            },
            receiver,
        )
    }

    fn capacity(&self) -> usize {
        self.buffer_capacity
    }
}

impl BytesChunkWriter {
    fn write(&mut self, s: &str) -> Result<(), Error> {
        self.write_buf(s.as_bytes())
    }

    fn write_buf(&mut self, mut buf: &[u8]) -> Result<(), Error> {
        let mut buffer = match self.buffer.take() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(self.buffer_capacity),
        };

        loop {
            let to_write = min(buf.len(), buffer.capacity() - buffer.len());
            let mut writer = buffer.writer();

            let sz = writer.write(&buf[..to_write])?;
            buffer = writer.into_inner();

            if buffer.capacity() - buffer.len() > 0 {
                self.buffer = Some(buffer);
                return Ok(());
            } else {
                self.sender.blocking_send(Ok(buffer.freeze()))?;

                if sz == buf.len() {
                    return Ok(());
                } else {
                    buf = &buf[sz..];
                    buffer = BytesMut::with_capacity(self.buffer_capacity);
                }
            }
        }
    }

    fn write_bytes(&mut self, bytes: Bytes) -> Result<(), Error> {
        let mut buffer = match self.buffer.take() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(self.buffer_capacity),
        };

        // large strings: send them directly
        if bytes.len() > self.buffer_capacity {
            if !buffer.is_empty() {
                self.sender.blocking_send(Ok(buffer.freeze()))?;
            }

            self.sender.blocking_send(Ok(bytes))?;
        } else {
            let mut buf = &bytes[..];

            loop {
                let to_write = min(buf.len(), buffer.capacity() - buffer.len());
                let mut writer = buffer.writer();

                let sz = writer.write(&buf[..to_write])?;
                buffer = writer.into_inner();

                if buffer.capacity() - buffer.len() > 0 {
                    self.buffer = Some(buffer);
                    return Ok(());
                } else {
                    self.sender.blocking_send(Ok(buffer.freeze()))?;

                    if sz == buf.len() {
                        return Ok(());
                    } else {
                        buf = &buf[sz..];
                        buffer = BytesMut::with_capacity(self.buffer_capacity);
                    }
                }
            }
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        if let Some(buffer) = self.buffer.take() {
            self.sender.blocking_send(Ok(buffer.freeze()))?;
        }

        Ok(())
    }
}

fn serialize_all(value: Value, w: &mut BytesChunkWriter) -> Result<(), Error> {
    serialize(value, w)?;
    w.flush()
}

fn serialize(value: Value, w: &mut BytesChunkWriter) -> Result<(), Error> {
    match value {
        Value::Null => w.write("null"),
        Value::Bool(b) => {
            if b {
                w.write("true")
            } else {
                w.write("false")
            }
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let mut buffer = itoa::Buffer::new();
                let s = buffer.format(i);

                w.write(s)
            } else if let Some(u) = n.as_u64() {
                let mut buffer = itoa::Buffer::new();
                let s = buffer.format(u);

                w.write(s)
            } else if let Some(f) = n.as_f64() {
                let mut buffer = ryu::Buffer::new();
                let s = buffer.format_finite(f);

                w.write(s)
            } else {
                Ok(())
            }
        }
        Value::String(s) => serialize_bytestring(s, w),
        Value::Array(a) => {
            w.write("[")?;

            let mut it = a.into_iter();

            if let Some(v) = it.next() {
                serialize(v, w)?;

                for v in it {
                    w.write(",")?;
                    serialize(v, w)?;
                }
            }

            w.write("]")
        }
        Value::Object(o) => {
            w.write("{")?;

            let mut it = o.into_iter();

            if let Some((key, v)) = it.next() {
                serialize_bytestring(key, w)?;

                w.write(":")?;
                serialize(v, w)?;

                for (key, v) in it {
                    w.write(",")?;

                    serialize_bytestring(key, w)?;
                    w.write(":")?;
                    serialize(v, w)?;
                }
            }

            w.write("}")
        }
    }
}

fn serialize_bytestring(s: ByteString, w: &mut BytesChunkWriter) -> Result<(), Error> {
    w.write("\"")?;

    let bytes = s.as_str().as_bytes();
    let mut start = 0;

    for (i, &byte) in bytes.iter().enumerate() {
        let escape = ESCAPE[byte as usize];
        if escape == 0 {
            continue;
        }

        if start < i {
            if i - start > w.capacity() {
                w.write_bytes(s.inner().slice(start..i))?;
            } else {
                w.write_buf(&bytes[start..i])?;
            }
        }

        let char_escape = from_escape_table(escape, byte);
        write_char_escape(char_escape, w)?;

        start = i + 1;
    }

    if start != bytes.len() {
        if bytes.len() - start > w.capacity() {
            w.write_bytes(s.inner().slice(start..))?;
        } else {
            w.write_buf(&bytes[start..])?;
        }
    }

    w.write("\"")
}

fn write_char_escape(char_escape: CharEscape, w: &mut BytesChunkWriter) -> Result<(), Error> {
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

            return w.write_buf(&bytes[..]);
        }
    };

    w.write_buf(&s[..])
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

pub enum Error {
    IO(std::io::Error),
    Send(SendError<Result<Bytes, io::Error>>),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IO(e)
    }
}

impl From<SendError<Result<Bytes, io::Error>>> for Error {
    fn from(e: SendError<Result<Bytes, io::Error>>) -> Self {
        Error::Send(e)
    }
}
