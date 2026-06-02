### Normalize `supergraph.path` to support queries with and without trailing slashes (`/`) ([PR #8860](https://github.com/apollographql/router/pull/8860))

Normalize trailing `/` for `supergraph.path` to support `/graphql` and `/graphql/`. This works by stripping trailing `/` from both the configured path and the incoming query path to ensure they match, regardless of whether the config or query includes a trailing slash.

By [@Jephuff](https://github.com/Jephuff) in https://github.com/apollographql/router/pull/8860