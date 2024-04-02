### Add support of instruments in configuration for telemetry ([Issue #4319](https://github.com/apollographql/router/issues/4319))

Add support for custom and standard instruments through the configuration file. You'll be able to add your own custom metrics just using the configuration file. They may:
- be conditional
- get values from selectors, for instance headers, context or body
- have different types like `histogram` or `counter`.

Example:

```yaml title="router.yaml"
telemetry:
  instrumentation:
    instruments:
      router:
        http.server.active_requests: true
        acme.request.duration:
          value: duration
          type: counter
          unit: kb
          description: "my description"
          attributes:
            http.response.status_code: true
            "my_attribute":
              response_header: "x-my-header"
  
      supergraph:
        acme.graphql.requests:
          value: unit
          type: counter
          unit: count
          description: "supergraph requests"
          
      subgraph:
        acme.graphql.subgraph.errors:
          value: unit
          type: counter
          unit: count
          description: "my description"
```

[Documentation](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/instruments)

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4771