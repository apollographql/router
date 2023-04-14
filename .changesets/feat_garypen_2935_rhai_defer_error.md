### Improve handling of deferred response errors from rhai scripts ([Issue #2935](https://github.com/apollographql/router/issues/2935))

Currently, if a rhai script errors in rhai, the rhai plugin ignores the error and returns None in the stream of results. This has two unfortunate aspects:

 - the error is not propagated to the client
 - the stream is terminated (silently)

The fix captures the error and propagates the response to the client.

This fix also adds support for the `is_primary()` API which may be invoked on both supergraph_service() and execution_service() responses. It may be used to avooid implementing exception handling for header interactions and to determine if a response `is_primary()` (i.e.: first) or not.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2945
