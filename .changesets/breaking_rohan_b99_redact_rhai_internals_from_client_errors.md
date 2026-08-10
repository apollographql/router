### Stop disclosing Rhai internals in client-facing error responses ([PR #9956](https://github.com/apollographql/router/pull/9956))

When a Rhai script failed, the router returned the raw Rhai error to the client, which exposed the fact that the router runs Rhai, the names of the script's callbacks, and the line and position where the failure happened:

```json
{
  "errors": [
    {
      "message": "rhai execution error: 'Runtime error: failed to convert header to a str (line 25, position 39)\nin call to function 'process_router_request' @ 'process_router_request' (line 6, position 29)'"
    }
  ]
}
```

Clients now only receive a message the script author chose. A `throw` is returned as written, with the Rhai wrapper stripped:

```rhai
throw "Invalid request";                          // client sees: Invalid request
throw #{ status: 403, message: "Forbidden" };     // client sees: Forbidden
throw #{ status: 403, body: #{ errors: [...] } }; // client sees the custom body
```

Anything the script did *not* choose is replaced with the status code's reason phrase, and the underlying error is logged at `ERROR` level instead. This covers failures inside the router's own Rhai functions (such as reading a header that isn't present), failures raised by the Rhai engine itself (such as calling an undefined function), and a `throw` carrying only a status - `throw #{ status: 400 }` now reads `Bad Request` rather than dumping the thrown object.

Three behaviour changes to be aware of when upgrading:

- Client-facing messages for a thrown string no longer include the `rhai execution error: 'Runtime error: ... (line N, position M)'` wrapper. Only the string you threw is returned. The full error is still in the logs.
- An error raised by a router Rhai function is now caught as a `RouterError` rather than a string. Interpolating it - `${err}` - reads as it always has, and `err.message` gives the same text. Errors your script raised with `throw` are caught unchanged.
- Re-throwing an error you caught keeps it redacted, whichever kind it is: a caught router error is opaque rather than an object map you can add a `status` or `message` to, and the object map the Rhai engine gives you for its own failures keeps its `message` out of the response. A `status` you set on that object map is still honoured; a caught router error takes no `status` at all - assigning one raises a fresh error that replaces the one you caught. To give a client a specific message, `throw` a new error instead.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9956
