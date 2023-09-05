### Handle multipart stream if the original stream is empty ([Issue #3293](https://github.com/apollographql/router/issues/3293))

For subscription and defer, in case the multipart response stream is empty then it should end correctly.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3748