### Support supergraph query selector for events ([PR #5764](https://github.com/apollographql/router/pull/5764))

The router now supports the `query: root_fields` selector for `event_response`. Previously the selector worked for `response` stage events but didn't work for `event_response`. 

The following configuration for a `query: root_fields` on an `event_response` now works:

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