### Stop disclosing Rhai internals in client-facing error responses ([PR #10004](https://github.com/apollographql/router/pull/10004))

When a Rhai script failed, the router wrapped the failure in its own error text before returning it to the client. That revealed that the router runs Rhai, the names of the script's callbacks, and the line and position of the failure:

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

Anything the script did *not* choose is replaced with the status code's reason phrase, and the underlying error is logged at `ERROR` level instead. This covers:

- Failures raised by the Rhai engine itself, such as calling an undefined function or a type mismatch.
- A `throw` carrying only a status: `throw #{ status: 400 }` now reads `Bad Request` rather than dumping the thrown object.
- A `throw` the router cannot read as a message: a value that is not a string or an object map, such as `throw 42`, or a map with an unreadable field, such as `throw #{ status: "four hundred", message: "Invalid request" }`. The whole map is discarded, so the `message` beside the bad status goes with it.

Errors raised by the router's own Rhai functions, such as `env::get()` on an unset variable or `base64::decode()` and `json::decode()` on malformed input, are no longer sent to clients. Clients see the status code's reason phrase, and the full error is logged.

Two things to be aware of when upgrading:

- Client-facing messages for a thrown string no longer include the `rhai execution error: 'Runtime error: ... (line N, position M)'` wrapper. Only the string you threw is returned. The full error is still in the logs.
- A `catch` block that catches an error from one of the router's Rhai functions now receives an error object instead of a string. `${err}` still gives the message, but `type_of(err)` and comparisons such as `err == "..."` change.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10004
