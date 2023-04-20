### Improve handling of deferred response errors from rhai scripts ([Issue #2935](https://github.com/apollographql/router/issues/2935)) ([Issue #2936](https://github.com/apollographql/router/issues/2936))


Whilst processing a deferred response; if a rhai script errors in rhai the router ignores the error and returns None in the stream of results. This has two unfortunate aspects:

 - the error is not propagated to the client
 - the stream is terminated (silently)

The fix captures the error and propagates the response to the client.

This fix also adds support for the `is_primary()` method which may be invoked on both supergraph_service() and execution_service() responses. It may be used to avoid implementing exception handling for header interactions and to determine if a response `is_primary()` (i.e.: first) or not.

e.g.:

```
    if response.is_primary() {
        print(`all response headers: ${response.headers}`);
    } else {
        print(`don't try to access headers`);
    }
```

vs

```
    try {
        print(`all response headers: ${response.headers}`);
    }
    catch(err) {
        if err == "cannot access headers on a deferred response" {
            print(`don't try to access headers`);
        }
    }
```
Note: This is a minimal example for purposes of illustration. A real exception handler would handle all error conditions.


By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2945
