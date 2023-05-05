### Fixes a bug preventing status code from making it out to the graphQL error extensions

> When contextually available, includes the HTTP status code with `SubrequestHttpError`. This provides plugins the ability to access the status code directly. Currently string parsing of the `reason` is the only way to determine the status.

Previous merge request added the status_code to the Error enum, but it was not serialized into the graphql error extensions which are available to plugins.

By [@scottdouglas1989](https://github.com/scottdouglas1989) in https://github.com/apollographql/router/pull/3005
