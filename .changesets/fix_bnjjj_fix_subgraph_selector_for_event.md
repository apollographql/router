### Evaluate selectors in response stage when possible ([PR #5725](https://github.com/apollographql/router/pull/5725))

As `events` are triggered at a specific event (`request`|`response`|`error`) we can only have condition for the related event, but sometimes selectors that can be applied at several events (like `subgraph_name` to get the subgraph name). Adds support for various supergraph selectors on response events.

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