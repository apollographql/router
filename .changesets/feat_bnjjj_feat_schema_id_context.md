### Add support of router request_context selector and add schema id in context ([PR #6160](https://github.com/apollographql/router/pull/6160))

Added a new selector `request_context` for the router service for telemetry. Now the supergraph schema id is also available through the context so if you want to access or display this schema id at the router service level you can now configure it like this:

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

It can be used in any services at any stages. This example is using selectors on event's attributes but it can also be done on spans and instruments.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6160