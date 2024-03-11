### Fix chunk formatting in multipart protocol ([Issue #4634](https://github.com/apollographql/router/issues/4634))

This PR changes the way we're sending chunks in the stream. Instead of finishing the chunk with `\r\n` we don't send this at the end of our current chunk but instead at the beginning of the next one. For the end users nothing changes but it let us to close the stream with the right final boundary by appending `--\r\n` directly to the last chunk.


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4681