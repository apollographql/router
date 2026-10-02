### Fix custom attributes on the `apollo.router.operations.subscriptions.terminated.client` subscription metric ([PR #9605](https://github.com/apollographql/router/pull/9605))

The telemetry for configuration `apollo.router.operations.subscriptions.terminated.client` accepts the same router selector syntax as other router instruments, but selector-based attributes were not actually applied at runtime.

You can now configure the default attributes (`reason`, `subgraph.name`, and `client.name`) and add custom attributes using any `RouterSelector`. For example, to include the operation name on every termination event:

```yaml
telemetry:
  instrumentation:
    instruments:
      router:
        apollo.router.operations.subscriptions.terminated.client:
          attributes:
            reason: true
            subgraph.name: true
            client.name: true
            graphql.operation.name:
              operation_name: string
```

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9605
