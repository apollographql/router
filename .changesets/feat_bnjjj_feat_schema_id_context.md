### Support new `request_context` selector for telemetry ([PR #6160](https://github.com/apollographql/router/pull/6160))

The router supports a new `request_context` selector for telemetry that enables access to the supergraph schema ID.

You can configure the context to access the supergraph schema ID at the router service level:

```yaml
telemetry:
  instrumentation:
    events:
      router:
        my.request_event:
          message: "my request event message"
          level: info
          on: request
          attributes:
            schema.id:
              request_context: "apollo::supergraph_schema_id" # The key containing the supergraph schema id
```

You can use the selector in any service at any stage. While this example applies to `events` attributes, the selector can also be used on spans and instruments.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6160