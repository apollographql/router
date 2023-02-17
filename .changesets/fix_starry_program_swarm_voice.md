### Metrics attributes allow value types as defined by otel ([Issue #2510](https://github.com/apollographql/router/issues/2510))

Metrics attributes in OpenTelemetry allow the following types:
* string
* string[]
* float
* float[]
* int
* int[]
* bool
* bool[]

However, our configuration only allowed strings. This has been fixed, and therefore it is now possible to use booleans via env expansion as metrics attributes.  

For example:
```yaml
telemetry:
  metrics:
    prometheus:
      enabled: true
    common:
      attributes:
        supergraph:
          static:
            - name: "my_boolean"
              value: '${env.MY_BOOLEAN:-true}'
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2616
