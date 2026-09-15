### Stop disclosing Rhai internals in client-facing error responses ([PR #10004](https://github.com/apollographql/router/pull/10004))

When a Rhai script failed, the router wrapped the failure in its own error text before returning it to the client, which exposed the fact that the router runs Rhai, the names of the script's callbacks, and the line and position where the failure happened:

```json
{
  "errors": [
    {
      "message": "rhai execution error: 'Runtime error: Invalid request (line 25, position 39)\nin call to function 'process_router_request' @ 'process_router_request' (line 6, position 29)'"
    }
  ]
}
```

Clients now receive only the message the script author chose. A thrown string is returned as written, with the Rhai wrapper stripped:

```rhai
throw "Invalid request";                          // client sees: Invalid request
throw #{ status: 403, message: "Forbidden" };     // client sees: Forbidden
throw #{ status: 403, body: #{ errors: [...] } }; // client sees the custom body
```

Anything the script did *not* choose is replaced with the status code's reason phrase, and the underlying error is logged at `ERROR` level instead. This covers failures raised by the Rhai engine itself (such as calling an undefined function or a type mismatch), a `throw` carrying only a status - `throw #{ status: 400 }` now reads `Bad Request` rather than dumping the thrown object - failures in the router's own Rhai functions that carry no message, such as reading a header that isn't present, and a `throw` the router cannot read as a message - a value that is not a string or an object map, such as `throw 42`, or a map with an unreadable field, such as `throw #{ status: "four hundred", message: "Invalid request" }`, which is discarded whole so the `message` beside the bad status goes with it.

Failures in the router's own Rhai functions that *do* carry a message are only partly covered: the wrapper, the script line and position and the chain of callbacks are gone, but the function's own message still reaches the client. `env::get()` on a variable that isn't set still reports `could not expand variable: MY_VAR, environment variable not found`, and `json::decode()` on malformed input still reports the parse error. A router function's error is indistinguishable from a script's own `throw`, so telling them apart would take recording which side raised it - the Rhai customization docs carry this as a documented limitation. If a script of yours calls those functions on a client-facing path, catch the error and throw your own.

Two things to be aware of when upgrading:

- Client-facing messages for a thrown string no longer include the `rhai execution error: 'Runtime error: ... (line N, position M)'` wrapper. Only the string you threw is returned. The full error is still in the logs.
- Nothing changes for scripts themselves: a `catch` block receives exactly what it received before, and status codes are unchanged.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10004
