### use a large enough buffer to flush in 

When writing a deferred response, if the output buffer was too small to write the entire compressed response, the compressor would write a small chunk, that did not decompress to the entire primary response, and would then wait for the next response to send the rest. Unfortunately, we cannot really know the output size we need in advance, and if we asked the decoder, it will tell us that it flushed all the data, even if it could have sent more.
So in here we raise the output buffer size, and do a second buffer growing step after flushing if necessary.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3067