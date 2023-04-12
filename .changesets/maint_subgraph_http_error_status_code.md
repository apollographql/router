### Adds HTTP status code to Subgraph HTTP error type

When contextually available, includes the HTTP status code with `SubrequestHttpError`. This provides plugins the ability to access the status code directly. Currently string parsing of the `reason` is the only way to determine the status.

By [@scottdouglas1989](https://github.com/scottdouglas1989) in https://github.com/apollographql/router/pull/2902
