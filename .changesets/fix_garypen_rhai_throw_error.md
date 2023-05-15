### Add support for throwing graphql errors in rhai responses ([Issue #3069](https://github.com/apollographql/router/issues/3069))

It's possible to throw a graphql error from rhai when processing a request. This extends the capability to include when processing a response.

Refer to the `Terminating client requests` section of the [Rhai api documentation](https://www.apollographql.com/docs/router/configuration/rhai) to learn how to throw GraphQL payloads.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3089