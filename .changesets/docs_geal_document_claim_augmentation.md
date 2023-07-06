### Document claim augmentation via coprocessors ([Issue #3102](https://github.com/apollographql/router/issues/3102))

Claims augmentation is a common use case where user information from the JWT claims is used to look up more context like roles from databases, before sending it to subgraphs. This can be done with subgraphs, but it was not documented yet, and there was confusion on the order in which the plugins were called. This clears the confusion and provides an example configuration.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3386