### Improve handling of deferred response errors from rhai scripts ([Issue #2935](https://github.com/apollographql/router/issues/2935))

Currently, if a rhai script errors in rhai, the rhai plugin ignores the error and returns None in the stream of results. This has two unfortunate aspects:

 - the error is not propagated to the client
 - the stream is terminated (silently)

The fix captures the error and propagates the response to the client.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2945