### Allow service version overrides ([PR #5689](https://github.com/apollographql/router/pull/5689))

The router now supports configuration of `service.version` via YAML file configuration. This enables users to produce custom versioned builds of the router.


The following example overrides the version to be `1.0`:
```yaml
telemetry:
  exporters:
    tracing:
      common:
        resource:
          service.version: 1.0
```


By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5689
