### Add `extensions.service` for all subgraph errors ([PR #6191](https://github.com/apollographql/router/pull/6191))


For improved debuggability, the router now supports adding a subgraph's name as an extension to all errors originating from the subgraph.

If `include_subgraph_errors` is `true` for a particular subgraph, all errors originating in this subgraph will have the subgraph's name exposed as a `service` extension.

You can enable subgraph errors with the following configuration:
```yaml title="router.yaml"
include_subgraph_errors:
  all: true # Propagate errors from all subgraphs
```
> Note: This option is enabled by default by the router's [dev mode](https://www.apollographql.com/docs/graphos/reference/router/configuration#dev-mode-defaults).

Consequently, when a subgraph returns an error, it will have a `service` extension with the subgraph name as its value. The following example shows the extension for a `products` subgraph:

```json
{
  "data": null,
  "errors": [
    {
      "message": "Invalid product ID",
      "path": [],
      "extensions": {
        "service": "products"
      }
    }
  ]
}
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6191