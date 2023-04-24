// All code from this module is extracted from https://github.com/Nemo157/async-compression and is under MIT or Apache-2 licence
// it will be removed when we find a long lasting solution to https://github.com/Nemo157/async-compression/issues/154
use std::io::Result;

use flate2::Compression;

use crate::axum_factory::compression::codec::Encode;
use crate::axum_factory::compression::codec::FlateEncoder;
use crate::axum_factory::compression::util::PartialBuffer;

#[derive(Debug)]
pub(crate) struct DeflateEncoder {
    inner: FlateEncoder,
}

impl DeflateEncoder {
    pub(crate) fn new(level: Compression) -> Self {
        Self {
            inner: FlateEncoder::new(level, false),
        }
    }
}

impl Encode for DeflateEncoder {
    fn encode(
        &mut self,
        input: &mut PartialBuffer<impl AsRef<[u8]>>,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<()> {
        self.inner.encode(input, output)
    }

    fn flush(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<bool> {
        self.inner.flush(output)
    }

    fn finish(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<bool> {
        self.inner.finish(output)
    }
}
