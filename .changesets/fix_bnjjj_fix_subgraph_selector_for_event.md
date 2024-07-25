### Evaluate selectors in response stage when possible ([PR #5725](https://github.com/apollographql/router/pull/5725))

As `events` are triggered at a specific stage (`request`|`response`|`error`) we can only have condition for the related stage and so sometimes you can also have selectors that can be applied at several stages (like `subgraph_name` to get the subgraph name). This change adds more possibilities when creating conditions on events.

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