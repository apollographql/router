### Fix compression for deferred responses ([Issue #1572](https://github.com/apollographql/router/issues/1572))

We replace tower-http's `CompressionLayer` with a custom stream transformation. This is necessary because tower-http uses async-compression, which buffers data until the end of the stream to then write it, ensuring a better compression. This is incompatible with the multipart protocol for `@defer`, which requires chunks to be sent as soon as possible. So we need to compress them independently.

This extracts parts of the codec module of async-compression, which so far is not public, and makes a streaming wrapper above it that flushes the compressed data on every response in the stream.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2986