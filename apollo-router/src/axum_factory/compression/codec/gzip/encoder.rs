// All code from this module is extracted from https://github.com/Nemo157/async-compression and is under MIT or Apache-2 licence
// it will be removed when we find a long lasting solution to https://github.com/Nemo157/async-compression/issues/154
use std::io::Result;

use flate2::Compression;
use flate2::Crc;

use crate::axum_factory::compression::codec::Encode;
use crate::axum_factory::compression::codec::FlateEncoder;
use crate::axum_factory::compression::util::PartialBuffer;

#[derive(Debug)]
enum State {
    Header(PartialBuffer<Vec<u8>>),
    Encoding,
    Footer(PartialBuffer<Vec<u8>>),
    Done,
}

#[derive(Debug)]
pub(crate) struct GzipEncoder {
    inner: FlateEncoder,
    crc: Crc,
    state: State,
}

fn header(level: Compression) -> Vec<u8> {
    let level_byte = if level.level() >= Compression::best().level() {
        0x02
    } else if level.level() <= Compression::fast().level() {
        0x04
    } else {
        0x00
    };

    vec![0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, level_byte, 0xff]
}

impl GzipEncoder {
    pub(crate) fn new(level: Compression) -> Self {
        Self {
            inner: FlateEncoder::new(level, false),
            crc: Crc::new(),
            state: State::Header(header(level).into()),
        }
    }

    fn footer(&mut self) -> Vec<u8> {
        let mut output = Vec::with_capacity(8);

        output.extend(&self.crc.sum().to_le_bytes());
        output.extend(&self.crc.amount().to_le_bytes());

        output
    }
}

impl Encode for GzipEncoder {
    fn encode(
        &mut self,
        input: &mut PartialBuffer<impl AsRef<[u8]>>,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<()> {
        loop {
            match &mut self.state {
                State::Header(header) => {
                    output.copy_unwritten_from(&mut *header);

                    if header.unwritten().is_empty() {
                        self.state = State::Encoding;
                    }
                }

                State::Encoding => {
                    let prior_written = input.written().len();
                    self.inner.encode(input, output)?;
                    self.crc.update(&input.written()[prior_written..]);
                }

                State::Footer(_) | State::Done => panic!("encode after complete"),
            };

            if input.unwritten().is_empty() || output.unwritten().is_empty() {
                return Ok(());
            }
        }
    }

    fn flush(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<bool> {
        loop {
            let done = match &mut self.state {
                State::Header(header) => {
                    output.copy_unwritten_from(&mut *header);

                    if header.unwritten().is_empty() {
                        self.state = State::Encoding;
                    }
                    false
                }

                State::Encoding => self.inner.flush(output)?,

                State::Footer(footer) => {
                    output.copy_unwritten_from(&mut *footer);

                    if footer.unwritten().is_empty() {
                        self.state = State::Done;
                        true
                    } else {
                        false
                    }
                }

                State::Done => true,
            };

            if done {
                return Ok(true);
            }

            if output.unwritten().is_empty() {
                return Ok(false);
            }
        }
    }

    fn finish(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<bool> {
        loop {
            match &mut self.state {
                State::Header(header) => {
                    output.copy_unwritten_from(&mut *header);

                    if header.unwritten().is_empty() {
                        self.state = State::Encoding;
                    }
                }

                State::Encoding => {
                    if self.inner.finish(output)? {
                        self.state = State::Footer(self.footer().into());
                    }
                }

                State::Footer(footer) => {
                    output.copy_unwritten_from(&mut *footer);

                    if footer.unwritten().is_empty() {
                        self.state = State::Done;
                    }
                }

                State::Done => {}
            };

            if let State::Done = self.state {
                return Ok(true);
            }

            if output.unwritten().is_empty() {
                return Ok(false);
            }
        }
    }
}
