### Add support of custom events defined by YAML for telemetry ([Issue #4320](https://github.com/apollographql/router/issues/4320))

Allow users to configure events via YAML configuration as documented [here](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/events/). This allows users to avoid reaching for rhai or a custom plugin when they want to log that something has happened (e.g. a request had errors of a particular type).

Here is an example of configuration:

```yaml
telemetry:
  instrumentation:
    events:
      router:
        # Standard events
        request: info
        response: info
        error: info

        # Custom events
        my.event:
          message: "my event message"
          level: info
          on: request
          attributes:
            http.response.body.size: false
          # Only log when the x-log-request header is `log` 
          condition:
            eq:
              - "log"
              - request_header: "x-log-request"
          
      supergraph:
          # Custom event configuration for supergraph service ...
      subgraph:
          # Custom event configuration for subgraph service .
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4956