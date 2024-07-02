### Add `operation_name` selector for router service in custom telemetry ([PR #5392](https://github.com/apollographql/router/pull/5392))

Adds an `operation_name` selector for the [router service](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors#router).
Previously, accessing `operation_name` was only possible through the `response_context` router service selector.

For example:

```yaml
telemetry:
  instrumentation:
    instruments:
      router:
        http.server.request.duration:
          attributes:
            graphql.operation.name:
              operation_name: string
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5392