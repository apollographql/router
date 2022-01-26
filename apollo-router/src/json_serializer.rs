use bytes::{BufMut, Bytes, BytesMut};
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::{ser::CharEscape, Serializer};
use serde_json_bytes::{ByteString, Value};
use std::{
    cmp::min,
    io::{self, Write},
};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::{self, error::SendError, Sender};
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

pub fn make_body2(value: Value) -> hyper::Body {
    let (mut write, read) = mpsc::channel(1024);
    tokio::task::spawn(async move { async_serialize2(value, &mut write).await });

    hyper::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(read))
}

fn async_serialize2(
    value: Value,
    sender: &mut Sender<Result<Bytes, io::Error>>,
) -> BoxFuture<Result<(), SendError<Result<Bytes, io::Error>>>> {
    Box::pin(async move {
        match value {
            Value::Null => {
                const null: Bytes = Bytes::from_static(b"null");
                sender.send(Ok(null)).await
            }
            Value::Bool(b) => {
                const true_bytes: Bytes = Bytes::from_static(b"true");
                const false_bytes: Bytes = Bytes::from_static(b"false");

                if b {
                    sender.send(Ok(true_bytes)).await
                } else {
                    sender.send(Ok(false_bytes)).await
                }
            }
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    let mut buffer = itoa::Buffer::new();
                    let s = buffer.format(i);

                    sender.send(Ok(s.to_string().into())).await
                } else if let Some(u) = n.as_u64() {
                    let mut buffer = itoa::Buffer::new();
                    let s = buffer.format(u);

                    sender.send(Ok(s.to_string().into())).await
                } else if let Some(f) = n.as_f64() {
                    let mut buffer = ryu::Buffer::new();
                    let s = buffer.format_finite(f);

                    sender.send(Ok(s.to_string().into())).await
                } else {
                    Ok(())
                }
            }
            Value::String(s) => async_serialize_bytestring2(s, sender).await,
            Value::Array(a) => {
                const array_open: Bytes = Bytes::from_static(b"[");
                const array_close: Bytes = Bytes::from_static(b"]");
                const comma: Bytes = Bytes::from_static(b",");

                sender.send(Ok(array_open)).await?;

                let mut it = a.into_iter();

                if let Some(v) = it.next() {
                    async_serialize2(v, sender).await?;

                    for v in it {
                        sender.send(Ok(comma)).await?;
                        async_serialize2(v, sender).await?;
                    }
                }

                sender.send(Ok(array_close)).await
            }
            Value::Object(o) => {
                const object_open: Bytes = Bytes::from_static(b"{");
                const object_close: Bytes = Bytes::from_static(b"}");
                const comma: Bytes = Bytes::from_static(b",");
                const colon: Bytes = Bytes::from_static(b":");

                sender.send(Ok(object_open)).await?;

                let mut it = o.into_iter();

                if let Some((key, v)) = it.next() {
                    async_serialize_bytestring2(key, sender).await?;
                    sender.send(Ok(colon)).await?;
                    async_serialize2(v, sender).await?;

                    for (key, v) in it {
                        sender.send(Ok(comma)).await?;

                        async_serialize_bytestring2(key, sender).await?;
                        sender.send(Ok(colon)).await?;
                        async_serialize2(v, sender).await?;
                    }
                }

                sender.send(Ok(object_close)).await
            }
        }
    })
}

async fn async_serialize_bytestring2(
    s: ByteString,
    sender: &mut Sender<Result<Bytes, io::Error>>,
) -> Result<(), SendError<Result<Bytes, io::Error>>> {
    const quotes: Bytes = Bytes::from_static(b"\"");
    sender.send(Ok(quotes)).await?;

    let bytes = s.as_str().as_bytes();
    let mut start = 0;

    for (i, &byte) in bytes.iter().enumerate() {
        let escape = ESCAPE[byte as usize];
        if escape == 0 {
            continue;
        }

        if start < i {
            sender.send(Ok(s.inner().slice(start..i))).await?;
        }

        let char_escape = from_escape_table(escape, byte);
        write_char_escape2(sender, char_escape).await?;

        start = i + 1;
    }

    if start != bytes.len() {
        sender.send(Ok(s.inner().slice(start..))).await?;
    }

    sender.send(Ok(quotes)).await
}

async fn write_char_escape2(
    sender: &mut Sender<Result<Bytes, io::Error>>,
    char_escape: CharEscape,
) -> Result<(), SendError<Result<Bytes, io::Error>>> {
    use CharEscape::*;

    let s = match char_escape {
        Quote => {
            const quotes: Bytes = Bytes::from_static(b"\\\"");
            quotes
        }
        ReverseSolidus => {
            const reverse: Bytes = Bytes::from_static(b"\\\\");
            reverse
        }
        Solidus => {
            const solidus: Bytes = Bytes::from_static(b"\\/");
            solidus
        }
        Backspace => {
            const backspace: Bytes = Bytes::from_static(b"\\b");
            backspace
        }
        FormFeed => {
            const formfeed: Bytes = Bytes::from_static(b"\\f");
            formfeed
        }
        LineFeed => {
            const linefeed: Bytes = Bytes::from_static(b"\\n");
            linefeed
        }
        CarriageReturn => {
            const cr: Bytes = Bytes::from_static(b"\\r");
            cr
        }
        Tab => {
            const tab: Bytes = Bytes::from_static(b"\\t");
            tab
        }
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
            return sender.send(Ok(Bytes::from((&bytes[..]).to_owned()))).await;
        }
    };

    sender.send(Ok(s)).await
}

pub fn make_body3(value: Value) -> hyper::Body {
    let mut v = Vec::new();
    sync_serialize3(value, &mut v);

    hyper::Body::wrap_stream(futures::stream::iter(v))
}

fn sync_serialize3(value: Value, queue: &mut Vec<Result<Bytes, io::Error>>) {
    match value {
        Value::Null => {
            const null: Bytes = Bytes::from_static(b"null");
            queue.push(Ok(null));
        }
        Value::Bool(b) => {
            const true_bytes: Bytes = Bytes::from_static(b"true");
            const false_bytes: Bytes = Bytes::from_static(b"false");

            if b {
                queue.push(Ok(true_bytes));
            } else {
                queue.push(Ok(false_bytes));
            }
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let mut buffer = itoa::Buffer::new();
                let s = buffer.format(i);

                queue.push(Ok(s.to_string().into()));
            } else if let Some(u) = n.as_u64() {
                let mut buffer = itoa::Buffer::new();
                let s = buffer.format(u);

                queue.push(Ok(s.to_string().into()));
            } else if let Some(f) = n.as_f64() {
                let mut buffer = ryu::Buffer::new();
                let s = buffer.format_finite(f);

                queue.push(Ok(s.to_string().into()));
            } else {
            }
        }
        Value::String(s) => sync_serialize_bytestring3(s, queue),
        Value::Array(a) => {
            const array_open: Bytes = Bytes::from_static(b"[");
            const array_close: Bytes = Bytes::from_static(b"]");
            const comma: Bytes = Bytes::from_static(b",");

            queue.push(Ok(array_open));

            let mut it = a.into_iter();

            if let Some(v) = it.next() {
                sync_serialize3(v, queue);

                for v in it {
                    queue.push(Ok(comma));
                    sync_serialize3(v, queue);
                }
            }

            queue.push(Ok(array_close));
        }
        Value::Object(o) => {
            const object_open: Bytes = Bytes::from_static(b"{");
            const object_close: Bytes = Bytes::from_static(b"}");
            const comma: Bytes = Bytes::from_static(b",");
            const colon: Bytes = Bytes::from_static(b":");

            queue.push(Ok(object_open));

            let mut it = o.into_iter();

            if let Some((key, v)) = it.next() {
                sync_serialize_bytestring3(key, queue);
                queue.push(Ok(colon));
                sync_serialize3(v, queue);

                for (key, v) in it {
                    queue.push(Ok(comma));

                    sync_serialize_bytestring3(key, queue);
                    queue.push(Ok(colon));
                    sync_serialize3(v, queue);
                }
            }

            queue.push(Ok(object_close));
        }
    }
}

fn sync_serialize_bytestring3(s: ByteString, queue: &mut Vec<Result<Bytes, io::Error>>) {
    const quotes: Bytes = Bytes::from_static(b"\"");
    queue.push(Ok(quotes));

    let bytes = s.as_str().as_bytes();
    let mut start = 0;

    for (i, &byte) in bytes.iter().enumerate() {
        let escape = ESCAPE[byte as usize];
        if escape == 0 {
            continue;
        }

        if start < i {
            queue.push(Ok(s.inner().slice(start..i)));
        }

        let char_escape = from_escape_table(escape, byte);
        write_char_escape3(queue, char_escape);

        start = i + 1;
    }

    if start != bytes.len() {
        queue.push(Ok(s.inner().slice(start..)));
    }

    queue.push(Ok(quotes));
}

fn write_char_escape3(queue: &mut Vec<Result<Bytes, io::Error>>, char_escape: CharEscape) {
    use CharEscape::*;

    let s = match char_escape {
        Quote => {
            const quotes: Bytes = Bytes::from_static(b"\\\"");
            quotes
        }
        ReverseSolidus => {
            const reverse: Bytes = Bytes::from_static(b"\\\\");
            reverse
        }
        Solidus => {
            const solidus: Bytes = Bytes::from_static(b"\\/");
            solidus
        }
        Backspace => {
            const backspace: Bytes = Bytes::from_static(b"\\b");
            backspace
        }
        FormFeed => {
            const formfeed: Bytes = Bytes::from_static(b"\\f");
            formfeed
        }
        LineFeed => {
            const linefeed: Bytes = Bytes::from_static(b"\\n");
            linefeed
        }
        CarriageReturn => {
            const cr: Bytes = Bytes::from_static(b"\\r");
            cr
        }
        Tab => {
            const tab: Bytes = Bytes::from_static(b"\\t");
            tab
        }
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
            return queue.push(Ok(Bytes::from((&bytes[..]).to_owned())));
        }
    };

    queue.push(Ok(s));
}

pub fn make_body4(value: Value) -> hyper::Body {
    let (mut acc, receiver) = BytesAcc::new(2048, 10);
    //let (mut write, read) = mpsc::channel(1024);
    tokio::task::spawn(async move {
        async_serialize4(value, &mut acc).await;
        acc.flush().await
    });

    hyper::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(receiver))
}

struct BytesAcc {
    sender: Sender<Result<Bytes, io::Error>>,
    buffer: Option<BytesMut>,
    buffer_capacity: usize,
}

impl BytesAcc {
    pub fn new(
        buffer_capacity: usize,
        channel_capacity: usize,
    ) -> (Self, mpsc::Receiver<Result<Bytes, io::Error>>) {
        let (sender, receiver) = mpsc::channel(channel_capacity);

        (
            BytesAcc {
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

const CAPA: usize = 2048;

impl BytesAcc {
    async fn write(&mut self, s: &str) {
        self.write_buf(s.as_bytes()).await;
    }

    async fn write_buf(&mut self, mut buf: &[u8]) {
        let mut buffer = match self.buffer.take() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(self.buffer_capacity),
        };

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
                //return Ok(size);
                return;
            } else {
                /*println!(
                    "=======> will send {}",
                    std::str::from_utf8(&buffer).unwrap()
                );*/
                self.sender.send(Ok(buffer.freeze())).await.unwrap();

                if sz == buf.len() {
                    //println!("wrote {} bytes", size);
                    //return Ok(size);
                    return;
                } else {
                    buf = &buf[sz..];
                    buffer = BytesMut::with_capacity(self.buffer_capacity);
                }
            }
        }
    }

    async fn write_bytes(&mut self, bytes: Bytes) {
        let mut buffer = match self.buffer.take() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(self.buffer_capacity),
        };

        // large strings: send them directly
        if bytes.len() > self.buffer_capacity {
            if !buffer.is_empty() {
                /*println!(
                    "=======> will send {}",
                    std::str::from_utf8(&buffer).unwrap()
                );*/
                self.sender.send(Ok(buffer.freeze())).await.unwrap();
                buffer = BytesMut::with_capacity(self.buffer_capacity);
            }

            /*println!(
                "=======> will send {}",
                std::str::from_utf8(&bytes).unwrap()
            );*/
            self.sender.send(Ok(bytes)).await.unwrap();
        } else {
            let mut buf = &bytes[..];

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
                    //return Ok(size);
                    return;
                } else {
                    /*println!(
                        "=======> will send {}",
                        std::str::from_utf8(&bytes).unwrap()
                    );*/
                    self.sender.send(Ok(buffer.freeze())).await.unwrap();

                    if sz == buf.len() {
                        //println!("wrote {} bytes", size);
                        //return Ok(size);
                        return;
                    } else {
                        buf = &buf[sz..];
                        buffer = BytesMut::with_capacity(self.buffer_capacity);
                    }
                }
            }
        }
    }

    async fn flush(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            /*println!(
                "=======> will send {}",
                std::str::from_utf8(&buffer).unwrap()
            );*/
            self.sender.send(Ok(buffer.freeze())).await.unwrap();
        }
    }
}

fn async_serialize4(value: Value, w: &mut BytesAcc) -> BoxFuture<()> {
    Box::pin(async move {
        match value {
            Value::Null => {
                w.write("null").await;
            }
            Value::Bool(b) => {
                if b {
                    w.write("true").await;
                } else {
                    w.write("false").await;
                }
            }
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    let mut buffer = itoa::Buffer::new();
                    let s = buffer.format(i);

                    w.write(s).await;
                } else if let Some(u) = n.as_u64() {
                    let mut buffer = itoa::Buffer::new();
                    let s = buffer.format(u);

                    w.write(s).await;
                } else if let Some(f) = n.as_f64() {
                    let mut buffer = ryu::Buffer::new();
                    let s = buffer.format_finite(f);

                    w.write(s).await;
                } else {
                }
            }
            Value::String(s) => async_serialize_bytestring4(s, w).await,
            Value::Array(a) => {
                w.write("[").await;

                let mut it = a.into_iter();

                if let Some(v) = it.next() {
                    async_serialize4(v, w).await;

                    for v in it {
                        w.write(",").await;
                        async_serialize4(v, w).await;
                    }
                }

                w.write("]").await;
            }
            Value::Object(o) => {
                w.write("{").await;

                let mut it = o.into_iter();

                if let Some((key, v)) = it.next() {
                    async_serialize_bytestring4(key, w).await;

                    w.write(":").await;
                    async_serialize4(v, w).await;

                    for (key, v) in it {
                        w.write(",").await;

                        async_serialize_bytestring4(key, w).await;
                        w.write(":").await;
                        async_serialize4(v, w).await;
                    }
                }

                w.write("}").await;
            }
        }
    })
}

async fn async_serialize_bytestring4(s: ByteString, w: &mut BytesAcc) {
    w.write("\"").await;

    let bytes = s.as_str().as_bytes();
    let mut start = 0;

    for (i, &byte) in bytes.iter().enumerate() {
        let escape = ESCAPE[byte as usize];
        if escape == 0 {
            continue;
        }

        if start < i {
            if i - start > w.capacity() {
                w.write_bytes(s.inner().slice(start..i)).await;
            } else {
                w.write_buf(&bytes[start..i]).await;
            }
        }

        let char_escape = from_escape_table(escape, byte);
        write_char_escape4(w, char_escape).await;

        start = i + 1;
    }

    if start != bytes.len() {
        if bytes.len() - start > w.capacity() {
            w.write_bytes(s.inner().slice(start..)).await;
        } else {
            w.write_buf(&bytes[start..]).await;
        }
    }

    w.write("\"").await;
}

async fn write_char_escape4(w: &mut BytesAcc, char_escape: CharEscape) {
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

            return w.write_buf(&bytes[..]).await;
        }
    };

    w.write_buf(&s[..]).await;
}

pub fn make_body5(value: Value) -> hyper::Body {
    let (mut acc, receiver) = BytesAcc2::new(2048, 10);
    //let (mut write, read) = mpsc::channel(1024);
    tokio::task::spawn_blocking(move || {
        sync_serialize5(value, &mut acc);
        acc.flush();
    });

    hyper::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(receiver))
}

struct BytesAcc2 {
    sender: Sender<Result<Bytes, io::Error>>,
    buffer: Option<BytesMut>,
    buffer_capacity: usize,
}

impl BytesAcc2 {
    pub fn new(
        buffer_capacity: usize,
        channel_capacity: usize,
    ) -> (Self, mpsc::Receiver<Result<Bytes, io::Error>>) {
        let (sender, receiver) = mpsc::channel(channel_capacity);

        (
            BytesAcc2 {
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

impl BytesAcc2 {
    fn write(&mut self, s: &str) {
        self.write_buf(s.as_bytes());
    }

    fn write_buf(&mut self, mut buf: &[u8]) {
        let mut buffer = match self.buffer.take() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(self.buffer_capacity),
        };

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
                //return Ok(size);
                return;
            } else {
                /*println!(
                    "=======> will send {}",
                    std::str::from_utf8(&buffer).unwrap()
                );*/
                self.sender.blocking_send(Ok(buffer.freeze())).unwrap();

                if sz == buf.len() {
                    //println!("wrote {} bytes", size);
                    //return Ok(size);
                    return;
                } else {
                    buf = &buf[sz..];
                    buffer = BytesMut::with_capacity(self.buffer_capacity);
                }
            }
        }
    }

    fn write_bytes(&mut self, bytes: Bytes) {
        let mut buffer = match self.buffer.take() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(self.buffer_capacity),
        };

        // large strings: send them directly
        if bytes.len() > self.buffer_capacity {
            if !buffer.is_empty() {
                /*println!(
                    "=======> will send {}",
                    std::str::from_utf8(&buffer).unwrap()
                );*/
                self.sender.blocking_send(Ok(buffer.freeze())).unwrap();
                buffer = BytesMut::with_capacity(self.buffer_capacity);
            }

            /*println!(
                "=======> will send {}",
                std::str::from_utf8(&bytes).unwrap()
            );*/
            self.sender.blocking_send(Ok(bytes)).unwrap();
        } else {
            let mut buf = &bytes[..];

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
                    //return Ok(size);
                    return;
                } else {
                    /*println!(
                        "=======> will send {}",
                        std::str::from_utf8(&bytes).unwrap()
                    );*/
                    self.sender.blocking_send(Ok(buffer.freeze())).unwrap();

                    if sz == buf.len() {
                        //println!("wrote {} bytes", size);
                        //return Ok(size);
                        return;
                    } else {
                        buf = &buf[sz..];
                        buffer = BytesMut::with_capacity(self.buffer_capacity);
                    }
                }
            }
        }
    }

    fn flush(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            /*println!(
                "=======> will send {}",
                std::str::from_utf8(&buffer).unwrap()
            );*/
            self.sender.blocking_send(Ok(buffer.freeze())).unwrap();
        }
    }
}

fn sync_serialize5(value: Value, w: &mut BytesAcc2) {
    match value {
        Value::Null => {
            w.write("null");
        }
        Value::Bool(b) => {
            if b {
                w.write("true");
            } else {
                w.write("false");
            }
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let mut buffer = itoa::Buffer::new();
                let s = buffer.format(i);

                w.write(s);
            } else if let Some(u) = n.as_u64() {
                let mut buffer = itoa::Buffer::new();
                let s = buffer.format(u);

                w.write(s);
            } else if let Some(f) = n.as_f64() {
                let mut buffer = ryu::Buffer::new();
                let s = buffer.format_finite(f);

                w.write(s);
            } else {
            }
        }
        Value::String(s) => sync_serialize_bytestring5(s, w),
        Value::Array(a) => {
            w.write("[");

            let mut it = a.into_iter();

            if let Some(v) = it.next() {
                sync_serialize5(v, w);

                for v in it {
                    w.write(",");
                    sync_serialize5(v, w);
                }
            }

            w.write("]");
        }
        Value::Object(o) => {
            w.write("{");

            let mut it = o.into_iter();

            if let Some((key, v)) = it.next() {
                sync_serialize_bytestring5(key, w);

                w.write(":");
                sync_serialize5(v, w);

                for (key, v) in it {
                    w.write(",");

                    sync_serialize_bytestring5(key, w);
                    w.write(":");
                    sync_serialize5(v, w);
                }
            }

            w.write("}");
        }
    }
}

fn sync_serialize_bytestring5(s: ByteString, w: &mut BytesAcc2) {
    w.write("\"");

    let bytes = s.as_str().as_bytes();
    let mut start = 0;

    for (i, &byte) in bytes.iter().enumerate() {
        let escape = ESCAPE[byte as usize];
        if escape == 0 {
            continue;
        }

        if start < i {
            if i - start > w.capacity() {
                w.write_bytes(s.inner().slice(start..i));
            } else {
                w.write_buf(&bytes[start..i]);
            }
        }

        let char_escape = from_escape_table(escape, byte);
        write_char_escape5(w, char_escape);

        start = i + 1;
    }

    if start != bytes.len() {
        if bytes.len() - start > w.capacity() {
            w.write_bytes(s.inner().slice(start..));
        } else {
            w.write_buf(&bytes[start..]);
        }
    }

    w.write("\"");
}

fn write_char_escape5(w: &mut BytesAcc2, char_escape: CharEscape) {
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

    w.write_buf(&s[..]);
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
