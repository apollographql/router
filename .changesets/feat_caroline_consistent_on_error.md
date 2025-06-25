### Align `on_graphql_error` selector return values with `subgraph_on_graphql_error` ([PR #7676](https://github.com/apollographql/router/pull/7676))

The `on_graphql_error` selector will now return `true` or `false`, in alignment with the `subgraph_on_graphql_error` selector. Previously, the selector would return `true` or `None`.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7676
