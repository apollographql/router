### Add static attribute on specific span in telemetry settings ([Issue #4561](https://github.com/apollographql/router/issues/4561))

Example of configuration:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          "my_attribute": "constant_value"
      supergraph:
        attributes:
          "my_attribute": "constant_value"
      subgraph:
        attributes:
          "my_attribute": "constant_value"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4566