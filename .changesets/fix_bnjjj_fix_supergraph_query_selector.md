### Execute supergraph query selector also on events ([PR #5764](https://github.com/apollographql/router/pull/5764))

The `query: root_fields` selector works on `response` stage for events right now but it should also work on `event_response`. This configuration is now working:

```yaml
telemetry:
  instrumentation:
    events:
      supergraph:
        OPERATION_LIMIT_INFO:
          message: operation limit info
          on: event_response
          level: info
          attributes:
            graphql.operation.name: true
            query.root_fields:
              query: root_fields
``` 

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5764