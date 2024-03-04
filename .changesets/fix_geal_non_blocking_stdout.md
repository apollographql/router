### use a non blocking stdout and stderr ([Issue #4612](https://github.com/apollographql/router/issues/4612))

If the router's output was piped into another process, and that process did not consume that output, it could entirely lock up the router. New connections were accepted, but requests never got an answer.
This is due to Rust protecting stdout and stderr access by a lock, to prevent multiple threads from interleaving their writes. When the process receiving the output from the router does not consume, then the logger's writes to the stream start to block, which means the current thread is blocked while holding the lock. And then any other thread that might want to log something will end up blocked too, waiting for that lock to be released.

This is fixed by  marking stdout and stderr as non blocking, which means that logs will be dropped silently when the buffer is full. This has another side effect that should be pointed out:
**if we write to stdout or sdtderr directly without handling errors (example: using `println!` or `eprintln!`) while the output is not consumed, then the router will panic. While that may look concerning, we consider that panicking, which will immediately reject the in flight requests and may trigger a restart of the router, is a better outcome than the router amking requests hang indefinitely.**

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4625