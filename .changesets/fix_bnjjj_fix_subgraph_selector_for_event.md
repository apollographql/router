### Evaluate selectors in response stage when possible ([PR #5725](https://github.com/apollographql/router/pull/5725))


The router now supports having various supergraph selectors on response events.

Because `events` are triggered at a specific event (`request`|`response`|`error`), you usually have only one condition for a related event. You can however have selectors that can be applied to several events, like `subgraph_name` to get the subgraph name). 

Example of an event to log the raw subgraph response only on a subgraph named `products`, this was not working before.

```yaml
telemetry:
  instrumentation:
    events:
      subgraph:
        response:
          level: info
          condition:
            eq:
            - subgraph_name: true
            - "products"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5725