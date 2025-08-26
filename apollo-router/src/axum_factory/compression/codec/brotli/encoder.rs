// All code from this module is extracted from https://github.com/Nemo157/async-compression and is under MIT or Apache-2 licence
// it will be removed when we find a long lasting solution to https://github.com/Nemo157/async-compression/issues/154
use std::fmt;
use std::io::Error;
use std::io::Result;

use brotli::enc::StandardAlloc;
use brotli::enc::backward_references::BrotliEncoderParams;
use brotli::enc::encode::BrotliEncoderOperation;
use brotli::enc::encode::BrotliEncoderStateStruct;

use crate::axum_factory::compression::codec::Encode;
use crate::axum_factory::compression::util::PartialBuffer;

pub(crate) struct BrotliEncoder {
    state: BrotliEncoderStateStruct<StandardAlloc>,
}

impl BrotliEncoder {
    pub(crate) fn new(params: BrotliEncoderParams) -> Self {
        let mut state = BrotliEncoderStateStruct::new(StandardAlloc::default());
        state.params = params;
        Self { state }
    }

    fn encode(
        &mut self,
        input: &mut PartialBuffer<impl AsRef<[u8]>>,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
        op: BrotliEncoderOperation,
    ) -> Result<()> {
        let in_buf = input.unwritten();
        let out_buf = output.unwritten_mut();

        let mut input_len = 0;
        let mut output_len = 0;

        if !self.state.compress_stream(
            op,
            &mut in_buf.len(),
            in_buf,
            &mut input_len,
            &mut out_buf.len(),
            out_buf,
            &mut output_len,
            &mut None,
            &mut |_, _, _, _| (),
        ) {
            return Err(Error::other("brotli error"));
        }

        input.advance(input_len);
        output.advance(output_len);

        Ok(())
    }
}

impl Encode for BrotliEncoder {
    fn encode(
        &mut self,
        input: &mut PartialBuffer<impl AsRef<[u8]>>,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<()> {
        self.encode(
            input,
            output,
            BrotliEncoderOperation::BROTLI_OPERATION_PROCESS,
        )
    }

    fn flush(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<bool> {
        self.encode(
            &mut PartialBuffer::new(&[][..]),
            output,
            BrotliEncoderOperation::BROTLI_OPERATION_FLUSH,
        )?;

        Ok(!self.state.has_more_output())
    }

    fn finish(
        &mut self,
        output: &mut PartialBuffer<impl AsRef<[u8]> + AsMut<[u8]>>,
    ) -> Result<bool> {
        self.encode(
            &mut PartialBuffer::new(&[][..]),
            output,
            BrotliEncoderOperation::BROTLI_OPERATION_FINISH,
        )?;

        Ok(self.state.is_finished())
    }
}

impl fmt::Debug for BrotliEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrotliEncoder")
            .field("compress", &"<no debug>")
            .finish()
    }
}
