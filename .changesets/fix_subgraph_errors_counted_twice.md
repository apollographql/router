### Count errors from several subgraph fetches only once in error metrics ([PR #10750](https://github.com/apollographql/router/pull/10750))

When more than one subgraph fetch of an operation returned errors, each fetch replaced the context's set of already-counted errors with its own, so only the last fetch's errors stayed in it. The execution layer then counted the other fetches' errors again, inflating `apollo.router.graphql_error` and `apollo.router.operations.error`.

Each subgraph fetch now adds its errors to the set instead of replacing it, so every error is counted once.

By [@Mohith26](https://github.com/Mohith26) in https://github.com/apollographql/router/pull/10750
