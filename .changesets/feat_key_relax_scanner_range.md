### GraphOS Enterprise: Coprocessor read access to request `uri`, `method` and HTTP response status codes ([Issue #2861](https://github.com/apollographql/router/issues/2861), [Issue #2861](https://github.com/apollographql/router/issues/2862))

We've added the ability for [coprocessors](https://www.apollographql.com/docs/router/customizations/coprocessor) to have read-only access to additional contextual information at [the `RouterService` and `SubgraphService`](https://www.apollographql.com/docs/router/customizations/coprocessor/#how-it-works) stages:

The `RouterService` stage now has read-only access to the **request** from the client:
  - `path` (e.g., `/graphql`)
  - `method` (e.g., `POST`, `GET`)

The `RouterService` stage now has read-only access to the overall **response** to the client:
  - `status_code` (e.g. `403`, `200`)

The `SubgraphService` stage now has read-only access to the **response** of the subgraph request:
  - `status_code` (e.g., `503`, `200`)

By [@o0ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/2863
