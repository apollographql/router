### Add `extensions.service` for all subgraph errors ([PR #6191](https://github.com/apollographql/router/pull/6191))

If `include_subgraph_errors` is `true` for a particular subgraph, all errors originating in this subgraph will have the subgraph's name exposed as a `service` extension.

For example, if subgraph errors are enabled, like so:
```yaml title="router.yaml"
include_subgraph_errors:
  all: true # Propagate errors from all subgraphs
```
Note: This option is enabled by default in the [dev mode](./configuration/overview#dev-mode-defaults).

And this `products` subgraph returns an error, it will have a `service` extension:

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