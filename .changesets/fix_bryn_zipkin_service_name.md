### Zipkin service name not populated ([Issue #4807](https://github.com/apollographql/router/issues/4807))

Zipkin trace exporter now respects service name configuration from yaml or environment variables.

For instance to set the service name to `my-app`, you can use the following configuration in your `router.yaml` file:
```yaml
telemetry:
  exporters:
    tracing:
      common:
        service_name: my-app
      zipkin:
        enabled: true
        endpoint: default
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4816
