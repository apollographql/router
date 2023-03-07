### Throw graphql errors in rhaiscript ([PR #2677](https://github.com/apollographql/router/pull/2677))

Up until now rhai script throws would yield an http status code and a message String which would end up as a GraphQL error.
This change allows users to throw with a valid GraphQL response body, which may include data, as well as errors and extensions.

Refer to the `Terminating client requests` section of the [Rhai api documentation](https://www.apollographql.com/docs/router/configuration/rhai) to learn how to throw GraphQL payloads.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2677
