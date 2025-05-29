### Add example Rhai script for returning Demand Control metrics as response headers ([PR #7564](https://github.com/apollographql/router/pull/7564))

Added a new section to the [demand control documentation](https://www.apollographql.com/docs/graphos/routing/security/demand-control#accessing-programmatically) showing how to use Rhai scripts to expose cost estimation data in response headers. This allows clients to see the estimated cost, actual cost, and other demand control metrics directly in HTTP responses, which is useful for debugging and client-side optimization.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/7564
