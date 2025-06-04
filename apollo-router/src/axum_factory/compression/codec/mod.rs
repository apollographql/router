// All code from this module is extracted from https://github.com/Nemo157/async-compression and is under MIT or Apache-2 licence
// it will be removed when we find a long lasting solution to https://github.com/Nemo157/async-compression/issues/154
use std::io::Result;

use super::util::PartialBuffer;

mod brotli;
mod deflate;
mod flate;
mod gzip;
//mod zlib;
mod zstd;

pub(crate) use self::brotli::BrotliEncoder;
pub(crate) use self::deflate::DeflateEncoder;
pub(crate) use self::flate::FlateEncoder;
pub(crate) use self::gzip::GzipEncoder;
pub(crate) use self::zstd::ZstdEncoder;

pub(crate) trait Encode {
    fn encode(
        &mut self,
        input: &mut PartialBuffer<impl AsRef<[u8]>>,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<()>;

    /// Returns whether the internal buffers are flushed
    fn flush(&mut self, output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>)
    -> Result<bool>;

    /// Returns whether the internal buffers are flushed and the end of the stream is written
    fn finish(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<bool>;
}
