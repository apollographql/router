### Fix Rhai engine faults leaking internal diagnostics to clients

When a Rhai script triggered an internal engine fault — calling an unknown function, a type mismatch, a stack overflow, or a similar error that can never come from the script's own `throw` statement — the router sent the client the raw Rhai diagnostic, including function names, script line and column numbers, and other implementation detail.

The router now returns a generic `internal server error` message for these faults and logs the full diagnostic, including the offending function and position, at `ERROR` level. Scripts that deliberately `throw` a custom message or a structured `#{ status, message, body }` object are unaffected: those reach the client exactly as before.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/TODO
