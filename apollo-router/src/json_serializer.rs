use bytes::{BufMut, Bytes, BytesMut};
use serde::Serialize;
use serde_json::Serializer;
use std::{
    cmp::min,
    io::{self, Write},
    str::from_utf8,
};
pub enum Error {
    IO(std::io::Error),
    Serde(serde_json::Error),
}

pub struct BytesWriter {
    sender: flume::Sender<Bytes>,
    buffer: Option<BytesMut>,
    buffer_capacity: usize,
}

impl BytesWriter {
    pub fn new(buffer_capacity: usize, channel_capacity: usize) -> (Self, flume::Receiver<Bytes>) {
        let (sender, receiver) = flume::bounded(channel_capacity);

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
        writer.flush().map_err(Error::IO)
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
                self.sender.send(buffer.freeze()).unwrap();

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
            self.sender.send(buffer.freeze()).unwrap();
        }
        Ok(())
    }
}
