### Stop leaking internal error details in supergraph `INTERNAL_SERVER_ERROR` responses

When an unhandled internal failure occurs while processing a request, the router puts the raw error string into the client-facing GraphQL response. These errors describe the router's internal state, such as supergraph and subgraph schema details, that the caller shouldn't have visibility into. Beyond being a security risk, these errors are not generally actionable by clients, so exposing them can often lead to confusion.

The router now returns a generic `internal server error` message for these responses while keeping the `INTERNAL_SERVER_ERROR` extension code and `500` status unchanged. The full error detail is logged server-side at `ERROR` level with `code = "INTERNAL_SERVER_ERROR"`, so operators retain the information for debugging. Internally, such failures now propagate as errors through the whole service pipeline and are handled by the router's existing transport-level internal-error handler; as a consequence, plugins and coprocessors no longer see (and no longer need to handle) response bodies for unhandled internal failures.

By [@TylerBloom](https://github.com/TylerBloom) in https://github.com/apollographql/router/pull/9999
